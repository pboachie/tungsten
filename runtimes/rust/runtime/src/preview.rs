// SPDX-License-Identifier: Apache-2.0
//! Previews: the rendered request,
//! the side effects in words and a confirmation token bound to the exact
//! arguments.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::client::ClientCore;
use crate::confirm::{CONFIRMATION_TTL_MS, issue_token};
use crate::envelope::Diag;
use crate::helpers::interpolate;
use crate::prepare::{Purpose, fail};
use crate::types::{
    CallOptions, Category, OperationDescriptor, PreviewMode, PreviewResult, RenderedRequest,
    Response, ResponseMeta, Result, Safety,
};
use crate::util::redact_paths;

pub(crate) fn local_meta() -> ResponseMeta {
    ResponseMeta {
        status: 0,
        headers: BTreeMap::new(),
        request_id: None,
        attempts: 0,
    }
}

impl ClientCore {
    pub(crate) async fn preview_inner(
        &self,
        op: &OperationDescriptor,
        args: &Value,
        opts: &CallOptions,
    ) -> Result<PreviewResult> {
        let prepared = self
            .prepare(op, args, opts, Purpose::Preview, None, None)
            .await?;
        let mut effects: Vec<String> = Vec::new();
        let confirmation = op.agent.confirmation.as_ref();
        if let Some(message) = confirmation
            .and_then(|c| c.message.as_deref())
            .filter(|m| !m.is_empty())
        {
            effects.push(interpolate(message, &prepared.args, op));
        }
        if let Some(note) = op
            .agent
            .remediation_note
            .as_deref()
            .filter(|n| !n.is_empty())
        {
            effects.push(note.to_owned());
        }
        if let Some(confirmation) = confirmation
            && !confirmation.summary_fields.is_empty()
        {
            let fields: Vec<String> = confirmation
                .summary_fields
                .iter()
                .map(|f| format!("{f}={{{f}}}"))
                .collect();
            effects.push(interpolate(
                &format!("Arguments: {}.", fields.join(", ")),
                &prepared.args,
                op,
            ));
        }
        if let Some(summary) = op.summary.as_deref().filter(|s| !s.is_empty()) {
            effects.push(summary.to_owned());
        }
        let token = (op.agent.safety != Safety::ReadOnly).then(|| {
            issue_token(
                &self.inner.confirmation_key,
                &op.id,
                &Value::Object(prepared.args.clone()),
                self.now(),
            )
        });
        let request = RenderedRequest {
            method: prepared.method,
            url: prepared.display_url.clone(),
            headers: prepared.headers.to_map(true),
            body: prepared.display_body.clone(),
        };
        let mut result = PreviewResult {
            operation: op.id.clone(),
            safety: op.agent.safety,
            request,
            expires_in_ms: token.as_ref().map(|_| CONFIRMATION_TTL_MS),
            confirmation_token: token,
            effects,
            server_preview: None,
            steps: None,
        };
        match &op.agent.preview {
            PreviewMode::Header { .. } => {
                let dry = self
                    .prepare(op, args, opts, Purpose::ServerPreview, None, None)
                    .await?;
                let answer = self.send(&dry, opts).await?;
                result.server_preview = answer
                    .value
                    .as_ref()
                    .map(|v| redact_paths(v, &op.agent.sensitive_response_fields));
                Ok(Response {
                    value: result,
                    meta: answer.meta,
                    verification: None,
                })
            }
            PreviewMode::Endpoint { operation } => {
                let Some(target) = self.operation(operation) else {
                    return Err(fail(
                        Diag::new(op.id.clone(), Category::ValidationFailed)
                            .failed_parameter("operation")
                            .expected(format!(
                                "the preview operation {operation} registered with the client"
                            ))
                            .remediation(format!(
                                "Register {operation} (ClientCore::register or ClientOptions::operations) so preview() can call it; nothing was sent."
                            ))
                            .build(),
                    ));
                };
                let mut rest = opts.clone();
                rest.confirm = None;
                rest.verify = false;
                let answer = self.call_with(&target, args, &rest, None, None).await?;
                result.server_preview = answer.value;
                Ok(Response {
                    value: result,
                    meta: answer.meta,
                    verification: None,
                })
            }
            PreviewMode::Local | PreviewMode::None => Ok(Response {
                value: result,
                meta: local_meta(),
                verification: None,
            }),
        }
    }
}
