//! `trackside-snapshot`: build the snapshot the MCP server serves.
//!
//! ```sh
//! trackside-snapshot --from 2026-09-22 --to 2026-09-29 --out snapshot.json.gz \
//!     [--upload s3://trackside-data-<account>/snapshots/latest.json.gz] [--archive-dir DIR]
//! ```
//!
//! Reads the racing archive read-only: `hr-<HR_ENV>-gamble-<account>` for fields, form and
//! results and `hr-<HR_ENV>-sectional-<account>` for the sectional canon (override with
//! `TRACKSIDE_RACING_BUCKET` / `TRACKSIDE_SECTIONAL_BUCKET`). When `TRACKSIDE_ROLE_ARN` is set
//! every AWS call runs as that role. `--archive-dir` reads a local mirror instead of S3.
//!
//! `trackside-snapshot --probe` runs the health probe described below from the command line.
//!
//! On Lambda (the scheduled daily refresh) it takes no arguments: it rebuilds from
//! `TRACKSIDE_SNAPSHOT_FROM` to a few days past today in Melbourne (`TRACKSIDE_SNAPSHOT_AHEAD`,
//! default 4, so upcoming fields are in), uploads to `TRACKSIDE_SNAPSHOT_UPLOAD`, and then
//! touches the MCP function named by `TRACKSIDE_MCP_FUNCTION` so new instances load the new
//! snapshot.
//!
//! The same function also runs an hourly health probe (event `{"probe": true}`): it checks
//! that the public MCP endpoint answers and asks for a token, that its sign-in metadata and
//! Cognito's keys are served, that the simulator page loads, and that the snapshot was
//! refreshed within `TRACKSIDE_PROBE_MAX_AGE_HOURS` (default 26). Any failure fails the
//! invocation, which the stack's alarm reports.

use std::io::Write;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use aws_sdk_s3::primitives::ByteStream;
use chrono::NaiveDate;

use trackside_ingest::archive::{Archive, Bucket, LocalArchive};

struct S3Archive {
    s3: aws_sdk_s3::Client,
    racing: String,
    sectional: String,
}

impl S3Archive {
    fn bucket(&self, b: Bucket) -> &str {
        match b {
            Bucket::Racing => &self.racing,
            Bucket::Sectional => &self.sectional,
        }
    }
}

#[async_trait]
impl Archive for S3Archive {
    async fn list(&self, bucket: Bucket, prefix: &str) -> Result<Vec<String>> {
        let mut pages = self
            .s3
            .list_objects_v2()
            .bucket(self.bucket(bucket))
            .prefix(prefix)
            .into_paginator()
            .send();
        let mut keys = Vec::new();
        while let Some(page) = pages.next().await {
            let page = page.with_context(|| format!("listing {prefix}"))?;
            keys.extend(
                page.contents()
                    .iter()
                    .filter_map(|o| o.key().map(str::to_string)),
            );
        }
        Ok(keys)
    }

    async fn get(&self, bucket: Bucket, key: &str) -> Result<Option<Vec<u8>>> {
        match self
            .s3
            .get_object()
            .bucket(self.bucket(bucket))
            .key(key)
            .send()
            .await
        {
            Ok(o) => Ok(Some(o.body.collect().await?.into_bytes().to_vec())),
            Err(e) if e.as_service_error().is_some_and(|e| e.is_no_such_key()) => Ok(None),
            Err(e) => Err(anyhow::Error::from(e).context(format!("reading {key}"))),
        }
    }
}

struct Args {
    from: NaiveDate,
    to: NaiveDate,
    out: String,
    upload: Option<String>,
    archive_dir: Option<String>,
}

fn args() -> Result<Args> {
    let mut from = None;
    let mut to = None;
    let mut out = "snapshot.json.gz".to_string();
    let mut upload = None;
    let mut archive_dir = None;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--from" => from = Some(value()?.parse()?),
            "--to" => to = Some(value()?.parse()?),
            "--out" => out = value()?,
            "--upload" => upload = Some(value()?),
            "--archive-dir" => archive_dir = Some(value()?),
            other => bail!("unknown flag {other}"),
        }
    }
    let to = to.unwrap_or_else(|| chrono::Utc::now().date_naive());
    let from = from.unwrap_or(to - chrono::Days::new(7));
    if from > to {
        bail!("--from {from} is after --to {to}");
    }
    Ok(Args {
        from,
        to,
        out,
        upload,
        archive_dir,
    })
}

async fn aws() -> Result<aws_config::SdkConfig> {
    let region = aws_config::Region::new(
        std::env::var("AWS_REGION").unwrap_or_else(|_| "ap-southeast-2".into()),
    );
    let base = aws_config::from_env().region(region.clone()).load().await;
    let Ok(role) = std::env::var("TRACKSIDE_ROLE_ARN") else {
        return Ok(base);
    };
    let provider = aws_config::sts::AssumeRoleProvider::builder(role)
        .session_name("trackside-snapshot")
        .region(region.clone())
        .configure(&base)
        .build()
        .await;
    Ok(aws_config::from_env()
        .region(region)
        .credentials_provider(provider)
        .load()
        .await)
}

/// `hr-<HR_ENV>-<kind>-<account>`, unless `var` names the bucket outright.
async fn archive_bucket(config: &aws_config::SdkConfig, var: &str, kind: &str) -> Result<String> {
    if let Ok(b) = std::env::var(var) {
        return Ok(b);
    }
    let env = std::env::var("HR_ENV").context("HR_ENV (or TRACKSIDE_*_BUCKET) must be set")?;
    let account = aws_sdk_sts::Client::new(config)
        .get_caller_identity()
        .send()
        .await
        .context("sts:GetCallerIdentity")?
        .account
        .context("no account in caller identity")?;
    Ok(format!("hr-{env}-{kind}-{account}"))
}

/// The daily refresh's window: a fixed start up to a few days past today in Melbourne.
fn lambda_args() -> Result<Args> {
    let from: NaiveDate = std::env::var("TRACKSIDE_SNAPSHOT_FROM")
        .context("TRACKSIDE_SNAPSHOT_FROM must be set")?
        .parse()
        .context("TRACKSIDE_SNAPSHOT_FROM must be YYYY-MM-DD")?;
    let ahead: u64 = std::env::var("TRACKSIDE_SNAPSHOT_AHEAD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);
    let today = chrono::Utc::now()
        .with_timezone(&chrono_tz::Australia::Melbourne)
        .date_naive();
    Ok(Args {
        from,
        to: today + chrono::Days::new(ahead),
        out: "/tmp/snapshot.json.gz".into(),
        upload: Some(
            std::env::var("TRACKSIDE_SNAPSHOT_UPLOAD")
                .context("TRACKSIDE_SNAPSHOT_UPLOAD must be set")?,
        ),
        archive_dir: None,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("AWS_LAMBDA_RUNTIME_API").is_err() {
        if std::env::args().nth(1).as_deref() == Some("--probe") {
            return probe().await.map(|_| ());
        }
        return build(args()?).await;
    }
    lambda_runtime::run(lambda_runtime::service_fn(
        |event: lambda_runtime::LambdaEvent<serde_json::Value>| async move {
            if event.payload.get("probe").and_then(|p| p.as_bool()) == Some(true) {
                return Ok(serde_json::json!({ "ok": true, "checked": probe().await? }));
            }
            build(lambda_args()?).await?;
            if let Ok(function) = std::env::var("TRACKSIDE_MCP_FUNCTION") {
                touch(&function).await?;
            }
            Ok::<_, lambda_runtime::Error>(serde_json::json!({ "ok": true }))
        },
    ))
    .await
    .map_err(|e| anyhow::anyhow!("lambda runtime: {e}"))
}

/// Checks what a judge would touch. Returns the checks that passed; the first failure is
/// the error.
async fn probe() -> Result<Vec<String>> {
    let base = std::env::var("TRACKSIDE_PROBE_URL").context("TRACKSIDE_PROBE_URL must be set")?;
    let base = base.trim_end_matches('/');
    let issuer =
        std::env::var("TRACKSIDE_PROBE_ISSUER").context("TRACKSIDE_PROBE_ISSUER must be set")?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let mut passed = Vec::new();

    // Without a token the MCP endpoint must refuse and point at its sign-in metadata.
    let mcp = http
        .post(format!("{base}/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .send()
        .await
        .context("MCP endpoint unreachable")?;
    let challenge = mcp
        .headers()
        .get("www-authenticate")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if mcp.status() != reqwest::StatusCode::UNAUTHORIZED || !challenge.contains("resource_metadata")
    {
        bail!(
            "MCP endpoint answered {} without a sign-in challenge",
            mcp.status()
        );
    }
    passed.push("mcp asks for a token".to_string());

    for (name, url, field) in [
        (
            "authorization metadata",
            format!("{base}/.well-known/oauth-authorization-server"),
            "token_endpoint",
        ),
        (
            "Cognito discovery",
            format!("{issuer}/.well-known/openid-configuration"),
            "jwks_uri",
        ),
        (
            "Cognito keys",
            format!("{issuer}/.well-known/jwks.json"),
            "keys",
        ),
    ] {
        let doc: serde_json::Value = http
            .get(&url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("{name} unreachable ({url})"))?
            .json()
            .await
            .with_context(|| format!("{name} is not JSON"))?;
        if doc.get(field).is_none() {
            bail!("{name} has no {field}");
        }
        passed.push(name.to_string());
    }

    if std::env::var("TRACKSIDE_PROBE_SIM").as_deref() == Ok("1") {
        http.get(format!("{base}/sim"))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .context("simulator page unreachable")?;
        passed.push("simulator page".to_string());
    }

    let target = std::env::var("TRACKSIDE_SNAPSHOT_UPLOAD")
        .context("TRACKSIDE_SNAPSHOT_UPLOAD must be set")?;
    let (bucket, key) = target
        .strip_prefix("s3://")
        .and_then(|t| t.split_once('/'))
        .context("TRACKSIDE_SNAPSHOT_UPLOAD must look like s3://bucket/key")?;
    let head = aws_sdk_s3::Client::new(&aws().await?)
        .head_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .with_context(|| format!("reading {target}"))?;
    let modified = head
        .last_modified()
        .context("snapshot has no last-modified time")?
        .secs();
    let max_hours: i64 = std::env::var("TRACKSIDE_PROBE_MAX_AGE_HOURS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(26);
    let now = chrono::Utc::now();
    let age_hours = (now.timestamp() - modified) / 3600;
    // One CloudWatch Embedded Metric Format line: Trackside/SnapshotAgeHours, graphed on the
    // stack's dashboard. Printed before the limit check so a stale snapshot is graphed too.
    println!(
        "{}",
        snapshot_age_emf(now.timestamp() - modified, now.timestamp_millis())
    );
    if age_hours > max_hours {
        bail!("snapshot is {age_hours} hours old (limit {max_hours}); the refresh is not running");
    }
    passed.push(format!("snapshot {age_hours}h old"));

    eprintln!("probe passed: {}", passed.join(", "));
    Ok(passed)
}

/// The snapshot's age as an EMF line in the `Trackside` namespace, with no dimensions.
fn snapshot_age_emf(age_secs: i64, timestamp_ms: i64) -> String {
    let hours = (age_secs as f64 / 3600.0 * 100.0).round() / 100.0;
    serde_json::json!({
        "_aws": {
            "Timestamp": timestamp_ms,
            "CloudWatchMetrics": [{
                "Namespace": "Trackside",
                "Dimensions": [[]],
                "Metrics": [{ "Name": "SnapshotAgeHours", "Unit": "None" }],
            }],
        },
        "SnapshotAgeHours": hours,
    })
    .to_string()
}

/// A configuration change retires the function's warm instances, so the next request loads
/// the snapshot just uploaded.
async fn touch(function: &str) -> Result<()> {
    let config = aws().await?;
    aws_sdk_lambda::Client::new(&config)
        .update_function_configuration()
        .function_name(function)
        .description(format!(
            "Trackside MCP server (snapshot refreshed {})",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        ))
        .send()
        .await
        .with_context(|| format!("refreshing {function}"))?;
    eprintln!("refreshed {function}");
    Ok(())
}

async fn build(args: Args) -> Result<()> {
    let dates: Vec<NaiveDate> = args
        .from
        .iter_days()
        .take_while(|d| *d <= args.to)
        .collect();

    let config = if args.archive_dir.is_none() || args.upload.is_some() {
        Some(aws().await?)
    } else {
        None
    };
    let archive: Box<dyn Archive> = match (&args.archive_dir, &config) {
        (Some(dir), _) => Box::new(LocalArchive { root: dir.into() }),
        (None, Some(config)) => Box::new(S3Archive {
            s3: aws_sdk_s3::Client::new(config),
            racing: archive_bucket(config, "TRACKSIDE_RACING_BUCKET", "gamble").await?,
            sectional: archive_bucket(config, "TRACKSIDE_SECTIONAL_BUCKET", "sectional").await?,
        }),
        (None, None) => unreachable!(),
    };

    let (fixture, reports) = trackside_ingest::snapshot::build(archive.as_ref(), &dates).await?;
    for r in &reports {
        eprintln!(
            "{}: {} meetings, {} races, {} runners, {} form, {} results, {} with sectionals{}",
            r.date,
            r.meetings,
            r.races,
            r.runners,
            r.form,
            r.results,
            r.sectional_highlights,
            if r.warnings.is_empty() {
                String::new()
            } else {
                format!(", {} warnings", r.warnings.len())
            }
        );
        for w in &r.warnings {
            eprintln!("  warning: {w}");
        }
    }

    let json = serde_json::to_vec(&fixture)?;
    let body = if args.out.ends_with(".gz") {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        gz.write_all(&json)?;
        gz.finish()?
    } else {
        json
    };
    std::fs::write(&args.out, &body).with_context(|| format!("writing {}", args.out))?;
    eprintln!(
        "wrote {} ({} meetings, {} horses' form, {} results, {} KB)",
        args.out,
        fixture.meetings.len(),
        fixture.form.len(),
        fixture.results.len(),
        body.len() / 1024
    );

    if let (Some(target), Some(config)) = (&args.upload, &config) {
        let (bucket, key) = target
            .strip_prefix("s3://")
            .and_then(|t| t.split_once('/'))
            .context("--upload must look like s3://bucket/key")?;
        if !bucket.starts_with("trackside-") {
            bail!("refusing to write outside Trackside's own buckets: {bucket}");
        }
        aws_sdk_s3::Client::new(config)
            .put_object()
            .bucket(bucket)
            .key(key)
            .content_type("application/json")
            .set_content_encoding(args.out.ends_with(".gz").then(|| "gzip".to_string()))
            .body(ByteStream::from(body))
            .send()
            .await
            .with_context(|| format!("uploading to {target}"))?;
        eprintln!("uploaded {target}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn snapshot_age_is_an_emf_line() {
        let v: serde_json::Value =
            serde_json::from_str(&super::snapshot_age_emf(5400, 1_700_000_000_000)).unwrap();
        assert_eq!(v["SnapshotAgeHours"].as_f64(), Some(1.5));
        let cw = &v["_aws"]["CloudWatchMetrics"][0];
        assert_eq!(cw["Namespace"], "Trackside");
        assert_eq!(cw["Metrics"][0]["Name"], "SnapshotAgeHours");
    }
}
