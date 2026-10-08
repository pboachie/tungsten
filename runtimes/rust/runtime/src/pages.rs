// SPDX-License-Identifier: Apache-2.0
//! Pagination: cursor, offset, page number and `Link` header styles.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;
use url::Url;

use crate::client::ClientCore;
use crate::envelope::{Diag, scrub_diagnostic};
use crate::helpers::{MAX_SAFE_INTEGER, arg_name, bounded};
use crate::prepare::MacroClaim;
use crate::transport::next_link;
use crate::types::{
    CallOptions, Category, Error, OperationDescriptor, Page, PaginationDescriptor, PathSegment,
    Response, Result, ValidateResponses, Validation,
};
use crate::util::{SecretSet, canonical_json, get_path_str};

/// Where a page iteration stands.
pub(crate) struct PageState {
    pub args: Value,
    pub override_url: Option<String>,
    pub previous_cursor: Option<Value>,
    pub done: bool,
    /// An error to report after the page that precedes it.
    pub pending_error: Option<Error>,
}

impl PageState {
    pub fn new(args: Value) -> Self {
        PageState {
            args,
            override_url: None,
            previous_cursor: None,
            done: false,
            pending_error: None,
        }
    }
}

/// An asynchronous page iterator: call [`Pages::next`] until it returns
/// `None`. After an error item it returns `None`.
pub struct Pages {
    core: ClientCore,
    pub(crate) op: Arc<OperationDescriptor>,
    opts: CallOptions,
    state: PageState,
}

impl std::fmt::Debug for Pages {
    /// The operation only: arguments and options can hold secrets.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pages")
            .field("operation", &self.op.id)
            .field("done", &self.state.done)
            .finish_non_exhaustive()
    }
}

impl Pages {
    pub(crate) fn new(
        core: ClientCore,
        op: Arc<OperationDescriptor>,
        args: Value,
        opts: CallOptions,
    ) -> Self {
        Pages {
            core,
            op,
            opts,
            state: PageState::new(args),
        }
    }

    pub async fn next(&mut self) -> Option<Result<Page<Value>>> {
        self.core
            .page_step(&self.op, &mut self.state, &self.opts, None)
            .await
    }

    /// Items typed as `T` (an item that does not decode is an
    /// `UNEXPECTED_RESPONSE` error item).
    pub fn typed<T: DeserializeOwned>(self) -> TypedPages<T> {
        TypedPages {
            inner: self,
            marker: std::marker::PhantomData,
        }
    }
}

/// [`Pages`] with typed items.
#[derive(Debug)]
pub struct TypedPages<T> {
    inner: Pages,
    marker: std::marker::PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned> TypedPages<T> {
    pub async fn next(&mut self) -> Option<Result<Page<T>>> {
        let page = self.inner.next().await?;
        Some(crate::dispatch::decode_page(&self.inner.op.id, page))
    }
}

fn as_number(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < MAX_SAFE_INTEGER {
        Value::from(n as i64)
    } else {
        serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number)
    }
}

impl ClientCore {
    /// The arguments object's value at `key` (by argument name).
    fn arg_at<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
        args.get(key)
    }

    fn set_arg(args: &mut Value, key: &str, value: Value) {
        if let Value::Object(map) = args {
            map.insert(key.to_owned(), value);
        }
    }

    /// Fetch the next page; `None` after the last page or the first error.
    pub(crate) async fn page_step(
        &self,
        op: &OperationDescriptor,
        st: &mut PageState,
        opts: &CallOptions,
        step: Option<&MacroClaim>,
    ) -> Option<Result<Page<Value>>> {
        if let Some(error) = st.pending_error.take() {
            st.done = true;
            return Some(Err(error));
        }
        if st.done {
            return None;
        }
        let response = match self
            .call_with(op, &st.args, opts, st.override_url.as_deref(), step)
            .await
        {
            Ok(response) => response,
            Err(error) => {
                st.done = true;
                return Some(Err(error));
            }
        };
        let body = response.value.clone().unwrap_or(Value::Null);
        let items_field = match &op.pagination {
            Some(
                PaginationDescriptor::Cursor { items_field, .. }
                | PaginationDescriptor::Offset { items_field, .. }
                | PaginationDescriptor::Page { items_field, .. }
                | PaginationDescriptor::LinkHeader { items_field },
            ) => items_field.as_str(),
            None => "",
        };
        let found = if items_field.is_empty() {
            Some(&body)
        } else {
            get_path_str(Some(&body), items_field)
        };
        let items: Vec<Value> = match found {
            Some(Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        };
        let mut next: Option<Value> = None;
        match &op.pagination {
            Some(PaginationDescriptor::Cursor {
                request_param,
                response_field,
                ..
            }) => {
                let cursor = get_path_str(Some(&body), response_field)
                    .filter(|c| !c.is_null() && c.as_str() != Some(""));
                next = cursor
                    .filter(|c| {
                        !st.previous_cursor
                            .as_ref()
                            .is_some_and(|p| canonical_json(p) == canonical_json(c))
                    })
                    .cloned();
                if let Some(cursor) = &next {
                    st.previous_cursor = Some(cursor.clone());
                    Self::set_arg(&mut st.args, &arg_name(op, request_param), cursor.clone());
                }
            }
            Some(PaginationDescriptor::Offset {
                offset_param,
                limit_param,
                ..
            }) => {
                let offset_key = arg_name(op, offset_param);
                let limit =
                    Self::arg_at(&st.args, &arg_name(op, limit_param)).and_then(Value::as_f64);
                let offset = bounded(
                    Self::arg_at(&st.args, &offset_key),
                    0.0,
                    0.0,
                    MAX_SAFE_INTEGER,
                );
                let short = limit.is_some_and(|l| (items.len() as f64) < l);
                if !(items.is_empty() || short) {
                    let value = as_number(offset + items.len() as f64);
                    Self::set_arg(&mut st.args, &offset_key, value.clone());
                    next = Some(value);
                }
            }
            Some(PaginationDescriptor::Page {
                page_param,
                size_param,
                ..
            }) => {
                let page_key = arg_name(op, page_param);
                let size =
                    Self::arg_at(&st.args, &arg_name(op, size_param)).and_then(Value::as_f64);
                let page = bounded(
                    Self::arg_at(&st.args, &page_key),
                    1.0,
                    0.0,
                    MAX_SAFE_INTEGER,
                );
                let short = size.is_some_and(|s| (items.len() as f64) < s);
                if !(items.is_empty() || short) {
                    let value = as_number(page + 1.0);
                    Self::set_arg(&mut st.args, &page_key, value.clone());
                    next = Some(value);
                }
            }
            Some(PaginationDescriptor::LinkHeader { .. }) => {
                if let Some(link) = next_link(response.meta.headers.get("link").map(String::as_str))
                {
                    let base = st
                        .override_url
                        .clone()
                        .or_else(|| self.inner.base_url.clone())
                        .unwrap_or_default();
                    let origin = Url::parse(&base).ok();
                    let resolved = origin.as_ref().and_then(|b| b.join(&link).ok());
                    match (origin, resolved) {
                        (Some(origin), Some(resolved)) if resolved.origin() == origin.origin() => {
                            let target = resolved.to_string();
                            st.override_url = Some(target.clone());
                            next = Some(Value::String(target));
                        }
                        _ => {
                            st.pending_error = Some(Error::new(
                                Diag::new(op.id.clone(), Category::UnexpectedResponse)
                                    .http_status(Some(response.meta.status))
                                    .request_id(response.meta.request_id.clone())
                                    .remediation("The next-page Link header points to another origin; it is not followed so credentials never leave the API's host. Stop paginating here.")
                                    .attempts(response.meta.attempts)
                                    .build(),
                            ));
                        }
                    }
                }
            }
            None => {}
        }
        if let Some(error) = self.check_items(op, items_field, &items, &response) {
            st.done = true;
            return Some(Err(error));
        }
        if next.is_none() {
            st.done = true;
        }
        Some(Ok(Response {
            value: Page { items, body, next },
            meta: response.meta,
            verification: None,
        }))
    }

    /// Validate page items with the operation's `page_item` validator
    /// (warn: report the first bad item; strict: fail on it).
    fn check_items(
        &self,
        op: &OperationDescriptor,
        items_field: &str,
        items: &[Value],
        response: &Response<Option<Value>>,
    ) -> Option<Error> {
        let mode = self.inner.validate_responses;
        let validator = op.page_item.as_ref()?;
        if mode == ValidateResponses::Off {
            return None;
        }
        for (index, item) in items.iter().enumerate() {
            let Validation::Invalid(issues) = crate::validate::judge(validator.as_ref(), item)
            else {
                continue;
            };
            let issue = issues.first();
            let message = issue.map_or("a valid item", |i| i.message.as_str());
            let mut path = format!(
                "response{}{}[{index}]",
                if items_field.is_empty() { "" } else { "." },
                items_field
            );
            if let Some(issue) = issue {
                for segment in &issue.path {
                    match segment {
                        PathSegment::Index(n) => path.push_str(&format!("[{n}]")),
                        PathSegment::Key(k) => path.push_str(&format!(".{k}")),
                    }
                }
            }
            let diagnostic = Diag::new(op.id.clone(), Category::UnexpectedResponse)
                .http_status(Some(response.meta.status))
                .request_id(response.meta.request_id.clone())
                .failed_parameter(path.clone())
                .expected(message)
                .remediation(format!(
                    "A page item does not match the API description at {path} ({message})."
                ))
                .attempts(response.meta.attempts)
                .build();
            let diagnostic = scrub_diagnostic(diagnostic, &SecretSet::default());
            if mode == ValidateResponses::Strict {
                return Some(Error::new(diagnostic));
            }
            self.emit(&diagnostic);
            return None;
        }
        None
    }
}
