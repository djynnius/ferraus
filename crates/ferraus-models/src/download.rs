//! Resumable, verified downloads with mirror fallback.
//!
//! * Partial data lives in `<dest>.part`; a retry sends `Range: bytes=<have>-` and appends
//!   when the server answers `206`, so completed ranges are never fetched twice.
//! * The finished file is hashed (SHA-256). A mismatch deletes the partial file and retries
//!   from the next mirror; a second mismatch fails with exit code 6 (`ModelDownload`).
//! * Network failures on every mirror fail with exit code 10 (`Network`).

use ferraus_core::{Error, ErrorKind, Result};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Attempts per URL while each attempt makes progress (resume after a dropped connection).
const RESUME_ATTEMPTS: usize = 5;

#[derive(Clone, Debug, Default)]
pub struct DownloadReport {
    pub url: String,
    /// Bytes received over the network in this call (excludes data already on disk).
    pub bytes_fetched: u64,
    /// Offset the first request resumed from (0 = fresh download).
    pub resumed_from: u64,
    pub sha256: String,
    pub size_bytes: u64,
    pub mismatches: usize,
}

/// Options for one download.
pub struct DownloadRequest<'a> {
    pub urls: &'a [String],
    /// Expected SHA-256 (hex). `None` skips verification (only for explicitly allowed
    /// unverified entries).
    pub sha256: Option<&'a str>,
    pub dest: &'a Path,
    /// Called with human-readable progress lines.
    pub log: &'a mut dyn FnMut(&str),
    /// Called between chunks; return an error to abort (cancellation).
    pub check_cancel: &'a dyn Fn() -> Result<()>,
}

enum FetchError {
    /// Worth retrying the same URL (connection dropped). `progressed` = bytes arrived.
    Retryable { progressed: bool, message: String },
    /// This URL will not work (404, TLS failure, bad Content-Range...).
    Permanent(String),
    /// Cancellation or local I/O failure: abort the whole download.
    Abort(Error),
}

pub fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_recv_body(None)
        .user_agent(format!("ferraus/{}", ferraus_core::FERRAUS_VERSION))
        .build()
        .into()
}

/// Fetch a small document (manifest, signature) fully into memory.
pub fn get_bytes(url: &str, limit: u64) -> Result<Vec<u8>> {
    let net = |m: String| Error::new(ErrorKind::Network, "network_error", format!("{url}: {m}"));
    let resp = agent().get(url).call().map_err(|e| net(e.to_string()))?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(net(format!("HTTP {status}")));
    }
    let mut out = Vec::new();
    resp.into_body()
        .into_reader()
        .take(limit)
        .read_to_end(&mut out)
        .map_err(|e| net(e.to_string()))?;
    Ok(out)
}

/// Download with resume, verification, and mirror fallback.
pub fn download(req: DownloadRequest<'_>) -> Result<DownloadReport> {
    if req.urls.is_empty() {
        return Err(Error::new(
            ErrorKind::ModelDownload,
            "no_download_url",
            format!("no download URL for {}", req.dest.display()),
        ));
    }
    if let Some(parent) = req.dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let part = part_path(req.dest);
    let agent = agent();
    let mut report = DownloadReport::default();
    let mut first_request = true;
    let mut last_net_error: Option<String> = None;
    // candidate order: each URL once; after a hash mismatch the next mirror (wrapping)
    let mut idx = 0usize;
    let mut tried_without_mismatch = 0usize;
    loop {
        if tried_without_mismatch >= req.urls.len() {
            let msg = last_net_error.unwrap_or_else(|| "every mirror failed".into());
            return Err(Error::new(
                ErrorKind::Network,
                "download_failed",
                format!("could not download {}: {msg}", req.dest.display()),
            )
            .with_hint("check the network connection and retry; partial data is kept and resumed"));
        }
        let url = &req.urls[idx % req.urls.len()];
        idx += 1;
        let mut attempts = 0;
        let fetched = loop {
            (req.check_cancel)()?;
            attempts += 1;
            let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            if first_request {
                report.resumed_from = have;
                first_request = false;
            }
            if have > 0 {
                (req.log)(&format!("resuming {url} from byte {have}"));
            } else {
                (req.log)(&format!("downloading {url}"));
            }
            match fetch_once(&agent, url, &part, have, &mut report.bytes_fetched, req.check_cancel)
            {
                Ok(()) => break true,
                Err(FetchError::Abort(e)) => return Err(e),
                Err(FetchError::Permanent(m)) => {
                    (req.log)(&format!("{url}: {m}"));
                    last_net_error = Some(format!("{url}: {m}"));
                    break false;
                }
                Err(FetchError::Retryable {
                    progressed,
                    message,
                }) => {
                    (req.log)(&format!("{url}: {message}"));
                    last_net_error = Some(format!("{url}: {message}"));
                    if !progressed || attempts >= RESUME_ATTEMPTS {
                        break false;
                    }
                }
            }
        };
        if !fetched {
            tried_without_mismatch += 1;
            continue;
        }
        let digest = sha256_file(&part)?;
        let ok = req
            .sha256
            .map(|want| want.eq_ignore_ascii_case(&digest))
            .unwrap_or(true);
        if ok {
            report.size_bytes = std::fs::metadata(&part)?.len();
            report.sha256 = digest;
            report.url = url.clone();
            std::fs::rename(&part, req.dest)?;
            return Ok(report);
        }
        let _ = std::fs::remove_file(&part);
        report.mismatches += 1;
        let want = req.sha256.unwrap_or_default();
        (req.log)(&format!(
            "checksum mismatch from {url} (got {digest}, want {want}); deleted the download"
        ));
        if report.mismatches >= 2 {
            return Err(Error::new(
                ErrorKind::ModelDownload,
                "checksum_mismatch",
                format!(
                    "{} failed SHA-256 verification twice (last from {url}: got {digest}, want {want})",
                    req.dest.display()
                ),
            )
            .with_hint("the mirror may be corrupted; retry later or report it"));
        }
        // a mismatch restarts the mirror rotation from the next URL
        tried_without_mismatch = 0;
    }
}

/// One HTTP request, appending to `part` from offset `have`.
fn fetch_once(
    agent: &ureq::Agent,
    url: &str,
    part: &Path,
    have: u64,
    fetched: &mut u64,
    check_cancel: &dyn Fn() -> Result<()>,
) -> std::result::Result<(), FetchError> {
    let mut req = agent.get(url);
    if have > 0 {
        req = req.header("Range", format!("bytes={have}-"));
    }
    let resp = match req.call() {
        Ok(r) => r,
        Err(ureq::Error::Io(e)) => {
            return Err(FetchError::Retryable {
                progressed: false,
                message: e.to_string(),
            })
        }
        Err(ureq::Error::Timeout(t)) => {
            return Err(FetchError::Retryable {
                progressed: false,
                message: format!("timeout ({t})"),
            })
        }
        Err(e) => return Err(FetchError::Permanent(e.to_string())),
    };
    let status = resp.status().as_u16();
    let append = match status {
        206 => {
            // Content-Range: bytes <start>-<end>/<total>
            let start = resp
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("bytes "))
                .and_then(|v| v.split('-').next())
                .and_then(|v| v.trim().parse::<u64>().ok());
            if start != Some(have) {
                // server ignored our offset: restart cleanly
                let _ = std::fs::remove_file(part);
                return Err(FetchError::Retryable {
                    progressed: true,
                    message: "server returned an unexpected Content-Range; restarting".into(),
                });
            }
            true
        }
        200 => false,
        416 if have > 0 => {
            // we already have everything the server has; let verification decide
            return Ok(());
        }
        s if s >= 500 => {
            return Err(FetchError::Retryable {
                progressed: false,
                message: format!("HTTP {s}"),
            })
        }
        s => return Err(FetchError::Permanent(format!("HTTP {s}"))),
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(part)
        .map_err(|e| FetchError::Abort(e.into()))?;
    let mut reader = resp.into_body().into_reader();
    let mut buf = vec![0u8; 1 << 16];
    let mut progressed = false;
    loop {
        check_cancel().map_err(FetchError::Abort)?;
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let _ = file.flush();
                return Err(FetchError::Retryable {
                    progressed,
                    message: format!("connection interrupted: {e}"),
                });
            }
        };
        file.write_all(&buf[..n])
            .map_err(|e| FetchError::Abort(e.into()))?;
        *fetched += n as u64;
        progressed = true;
    }
    file.flush().map_err(|e| FetchError::Abort(e.into()))?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_server {
    //! A tiny HTTP/1.1 file server with Range support and fault injection.

    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug)]
    pub struct Hit {
        pub path: String,
        pub range_start: Option<u64>,
        pub bytes_sent: u64,
    }

    #[derive(Clone)]
    pub enum Behavior {
        /// Serve the bytes, honouring Range.
        Serve(Vec<u8>),
        /// First request: send only `cut` bytes then drop the connection. Later: serve.
        DropOnceAfter(Vec<u8>, usize),
        NotFound,
    }

    pub struct Server {
        pub base: String,
        pub hits: Arc<Mutex<Vec<Hit>>>,
    }

    /// Start a server; `routes` maps a path (`/a.bin`) to a behavior.
    pub fn start(routes: Vec<(&'static str, Behavior)>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(Mutex::new(Vec::new()));
        let hits2 = hits.clone();
        let dropped = Arc::new(Mutex::new(std::collections::HashSet::<String>::new()));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let mut range_start = None;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                        break;
                    }
                    let lower = h.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("range: bytes=") {
                        range_start = v.trim().trim_end_matches('-').parse::<u64>().ok();
                    }
                }
                let behavior = routes
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map(|(_, b)| b.clone())
                    .unwrap_or(Behavior::NotFound);
                let (data, cut) = match behavior {
                    Behavior::NotFound => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                        hits2.lock().unwrap().push(Hit {
                            path,
                            range_start,
                            bytes_sent: 0,
                        });
                        continue;
                    }
                    Behavior::Serve(d) => (d, None),
                    Behavior::DropOnceAfter(d, cut) => {
                        let first = dropped.lock().unwrap().insert(path.clone());
                        (d, if first { Some(cut) } else { None })
                    }
                };
                let start = range_start.unwrap_or(0).min(data.len() as u64) as usize;
                let body = &data[start..];
                let head = if range_start.is_some() {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                        body.len(),
                        start,
                        data.len().saturating_sub(1),
                        data.len()
                    )
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                };
                let _ = stream.write_all(head.as_bytes());
                let send = cut.map(|c| c.min(body.len())).unwrap_or(body.len());
                let _ = stream.write_all(&body[..send]);
                let _ = stream.flush();
                hits2.lock().unwrap().push(Hit {
                    path,
                    range_start,
                    bytes_sent: send as u64,
                });
                // dropping the stream closes the connection (early when cut)
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        });
        Server { base, hits }
    }
}

#[cfg(test)]
mod tests {
    use super::test_server::{start, Behavior};
    use super::*;

    fn payload(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 31 % 251) as u8).collect()
    }

    fn sha(d: &[u8]) -> String {
        hex::encode(Sha256::digest(d))
    }

    fn run(urls: Vec<String>, want: &str, dest: &Path) -> Result<DownloadReport> {
        let mut log = |_: &str| {};
        download(DownloadRequest {
            urls: &urls,
            sha256: Some(want),
            dest,
            log: &mut log,
            check_cancel: &|| Ok(()),
        })
    }

    #[test]
    fn resumes_after_interrupted_transfer_without_refetching() {
        let data = payload(300_000);
        let srv = start(vec![("/m.onnx", Behavior::DropOnceAfter(data.clone(), 120_000))]);
        let dir = ferraus_core::testing::TempDir::new("dl-resume");
        let dest = dir.path("model.onnx");
        let rep = run(vec![format!("{}/m.onnx", srv.base)], &sha(&data), &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        let hits = srv.hits.lock().unwrap().clone();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].range_start, None);
        assert_eq!(hits[0].bytes_sent, 120_000);
        // the second request resumed exactly where the first stopped
        assert_eq!(hits[1].range_start, Some(120_000));
        assert_eq!(hits[1].bytes_sent, 180_000);
        assert_eq!(rep.bytes_fetched, 300_000, "no byte fetched twice");
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn resumes_from_existing_partial_file() {
        let data = payload(50_000);
        let srv = start(vec![("/m.onnx", Behavior::Serve(data.clone()))]);
        let dir = ferraus_core::testing::TempDir::new("dl-partial");
        let dest = dir.path("model.onnx");
        std::fs::write(part_path(&dest), &data[..20_000]).unwrap();
        let rep = run(vec![format!("{}/m.onnx", srv.base)], &sha(&data), &dest).unwrap();
        assert_eq!(rep.resumed_from, 20_000);
        assert_eq!(rep.bytes_fetched, 30_000);
        assert_eq!(std::fs::read(&dest).unwrap(), data);
    }

    #[test]
    fn mismatch_is_deleted_and_retried_from_mirror() {
        let data = payload(10_000);
        let mut corrupt = data.clone();
        corrupt[5_000] ^= 0xFF;
        let srv = start(vec![
            ("/bad.onnx", Behavior::Serve(corrupt)),
            ("/good.onnx", Behavior::Serve(data.clone())),
        ]);
        let dir = ferraus_core::testing::TempDir::new("dl-mirror");
        let dest = dir.path("model.onnx");
        let rep = run(
            vec![
                format!("{}/bad.onnx", srv.base),
                format!("{}/good.onnx", srv.base),
            ],
            &sha(&data),
            &dest,
        )
        .unwrap();
        assert_eq!(rep.mismatches, 1);
        assert!(rep.url.ends_with("/good.onnx"));
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        // the mirror download started from scratch (corrupt bytes were deleted)
        let hits = srv.hits.lock().unwrap().clone();
        assert_eq!(hits[1].range_start, None);
    }

    #[test]
    fn second_mismatch_is_exit_code_6() {
        let data = payload(4_000);
        let mut corrupt = data.clone();
        corrupt[0] ^= 1;
        let srv = start(vec![
            ("/a.onnx", Behavior::Serve(corrupt.clone())),
            ("/b.onnx", Behavior::Serve(corrupt)),
        ]);
        let dir = ferraus_core::testing::TempDir::new("dl-mismatch");
        let dest = dir.path("model.onnx");
        let e = run(
            vec![format!("{}/a.onnx", srv.base), format!("{}/b.onnx", srv.base)],
            &sha(&data),
            &dest,
        )
        .unwrap_err();
        assert_eq!(e.kind, ErrorKind::ModelDownload);
        assert_eq!(e.exit_code, 6);
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn network_failure_is_exit_code_10() {
        let srv = start(vec![]);
        let dir = ferraus_core::testing::TempDir::new("dl-404");
        let dest = dir.path("model.onnx");
        // 404 on the first, refused connection on the second
        let e = run(
            vec![
                format!("{}/missing.onnx", srv.base),
                "http://127.0.0.1:9/never".to_string(),
            ],
            &"0".repeat(64),
            &dest,
        )
        .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Network);
        assert_eq!(e.exit_code, 10);
    }

    #[test]
    fn falls_back_to_mirror_on_404() {
        let data = payload(1_000);
        let srv = start(vec![("/ok.onnx", Behavior::Serve(data.clone()))]);
        let dir = ferraus_core::testing::TempDir::new("dl-fallback");
        let dest = dir.path("model.onnx");
        let rep = run(
            vec![format!("{}/gone.onnx", srv.base), format!("{}/ok.onnx", srv.base)],
            &sha(&data),
            &dest,
        )
        .unwrap();
        assert!(rep.url.ends_with("/ok.onnx"));
    }
}
