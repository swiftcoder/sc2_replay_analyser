use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::model::{EventRecord, MetricsRecord, Summary};

fn write_line<W: Write, T: serde::Serialize>(writer: &mut W, value: &T) -> Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    Ok(())
}

pub struct MetricsWriter {
    writer: BufWriter<File>,
    path: PathBuf,
}

impl MetricsWriter {
    pub fn create(dir: &Path, header: MetricsRecord) -> Result<Self> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating output dir {}", dir.display()))?;
        let path = dir.join("metrics.jsonl");
        let file = File::create(&path)
            .with_context(|| format!("creating {}", path.display()))?;
        let mut writer = BufWriter::new(file);
        write_line(&mut writer, &header)?;
        Ok(Self { writer, path })
    }

    pub fn push(&mut self, record: MetricsRecord) -> Result<()> {
        write_line(&mut self.writer, &record)
    }

    pub fn finish(mut self, footer: MetricsRecord) -> Result<PathBuf> {
        write_line(&mut self.writer, &footer)?;
        self.writer.flush()?;
        Ok(self.path)
    }
}

pub struct EventsWriter {
    writer: BufWriter<File>,
    path: PathBuf,
}

impl EventsWriter {
    pub fn create(dir: &Path, header: EventRecord) -> Result<Self> {
        let path = dir.join("events.jsonl");
        let file = File::create(&path).context("creating events.jsonl")?;
        let mut writer = BufWriter::new(file);
        write_line(&mut writer, &header)?;
        Ok(Self { writer, path })
    }

    pub fn push(&mut self, record: EventRecord) -> Result<()> {
        write_line(&mut self.writer, &record)
    }

    pub fn finish(mut self, footer: EventRecord) -> Result<PathBuf> {
        write_line(&mut self.writer, &footer)?;
        self.writer.flush()?;
        Ok(self.path)
    }
}

pub fn write_summary(dir: &Path, summary: &Summary) -> Result<PathBuf> {
    let path = dir.join("summary.json");
    let file = File::create(&path).context("creating summary.json")?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, summary)?;
    writer.flush()?;
    Ok(path)
}
