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

#[tokio::main]
async fn main() -> Result<()> {
    let args = args()?;
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
