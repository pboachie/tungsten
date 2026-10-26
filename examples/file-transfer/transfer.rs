// SPDX-License-Identifier: AGPL-3.0-only
//! Stream a file up and down through the generated Rust SDK.
//!
//! The runner builds this as a small binary and runs it with `BASE_URL`.
use std::io::Write;

use files_sdk::FilesClient;
use files_sdk::resources::files::{FilesGetRequest, FilesUpdateRequest};
use sha2::{Digest, Sha256};
use tungsten_runtime::{
    Binary, CallOptions, ClientOptions, DownloadOptions, UploadReader, UploadSource,
};

const SIZE: usize = 16 * 1024 * 1024;

fn hex(digest: impl AsRef<[u8]>) -> String {
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::args().nth(1).ok_or("usage: transfer BASE_URL")?;

    // A 16 MiB input file, written in pieces.
    let mut digest = Sha256::new();
    let mut out = std::fs::File::create("input.bin")?;
    for i in 0..SIZE / 65536 {
        let piece = vec![(i % 251) as u8; 65536];
        digest.update(&piece);
        out.write_all(&piece)?;
    }
    drop(out);
    let sha256 = hex(digest.finalize());

    let client = FilesClient::new(ClientOptions {
        base_url: Some(base),
        ..Default::default()
    })?;
    let opts = CallOptions::default();
    // The file, opened again from the start for every attempt.
    let source = UploadSource::recreatable(|| {
        let file = std::fs::read("input.bin")?;
        Ok(Box::new(std::io::Cursor::new(file)) as UploadReader)
    })
    .with_length(SIZE as u64)?
    .with_sha256(&sha256)?;
    // The request's body is replaced by the source.
    let request = FilesUpdateRequest::new("report.bin".into(), Binary::new(Vec::new()));
    let stored = client
        .files()
        .update_upload(request, source, &opts)
        .await
        .map_err(|e| format!("upload failed: {}", e.diagnostic.remediation))?
        .value;
    assert_eq!(stored.sha256, sha256, "the server stored different bytes");
    println!("rust: uploaded {} bytes, sha256 matches", stored.bytes);

    let _ = std::fs::remove_file("copy.bin");
    let get = || FilesGetRequest::new("report.bin".into());
    let download = client
        .files()
        .get_download(get(), &opts, DownloadOptions::default())
        .await
        .map_err(|e| format!("download failed: {}", e.diagnostic.remediation))?;
    // The server suggests a name; the caller still chooses the destination.
    assert_eq!(download.filename(), Some("report.bin"));
    let saved = download
        .save_to("copy.bin", false)
        .await
        .map_err(|e| format!("save failed: {}", e.diagnostic.remediation))?;
    assert_eq!(hex(Sha256::digest(std::fs::read("copy.bin")?)), sha256, "the copy differs");
    println!("rust: downloaded {saved} bytes to copy.bin, sha256 matches");

    let again = client
        .files()
        .get_download(get(), &opts, DownloadOptions::default())
        .await
        .map_err(|e| format!("download failed: {}", e.diagnostic.remediation))?;
    let refused = again.save_to("copy.bin", false).await.err().and_then(|e| e.diagnostic.code);
    println!("rust: second save refused: {}", refused.unwrap_or_default());

    let capped = DownloadOptions {
        max_bytes: Some(1024 * 1024),
        ..Default::default()
    };
    let refused = client.files().get_download(get(), &opts, capped).await.err();
    println!(
        "rust: capped download refused: {}",
        refused.and_then(|e| e.diagnostic.code).unwrap_or_default()
    );
    let missing = client
        .files()
        .get_download(FilesGetRequest::new("missing.bin".into()), &opts, DownloadOptions::default())
        .await
        .err();
    let category = missing.map(|e| serde_json::to_value(e.diagnostic.category));
    println!("rust: missing file: {}", category.and_then(Result::ok).and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default());
    Ok(())
}
