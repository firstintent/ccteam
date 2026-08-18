//! In-memory sid to project/session metadata catalog.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use anyhow::Result;
use ccteam_harness::{read_session_meta, write_session_meta, SessionMeta};

#[derive(Debug, Clone)]
pub(crate) struct CatalogEntry {
    pub(crate) project: String,
    pub(crate) project_dir: PathBuf,
    pub(crate) meta: SessionMeta,
}

#[derive(Default)]
pub(crate) struct SessionCatalog {
    entries: RwLock<HashMap<String, CatalogEntry>>,
    disk_reads: AtomicU64,
}

impl SessionCatalog {
    pub(crate) fn get(&self, sid: &str) -> Option<CatalogEntry> {
        self.entries
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(sid)
            .cloned()
    }

    pub(crate) fn insert(&self, project_dir: &Path, meta: &SessionMeta) {
        self.entries
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                meta.sid.clone(),
                CatalogEntry {
                    project: meta.slug.clone(),
                    project_dir: project_dir.to_path_buf(),
                    meta: meta.clone(),
                },
            );
    }

    pub(crate) fn write(&self, project_dir: &Path, meta: &SessionMeta) -> Result<()> {
        write_session_meta(project_dir, meta)?;
        self.insert(project_dir, meta);
        Ok(())
    }

    pub(crate) fn find_or_load(
        &self,
        sid: &str,
        projects: &BTreeMap<String, PathBuf>,
    ) -> Option<CatalogEntry> {
        if let Some(entry) = self.get(sid) {
            return Some(entry);
        }
        for (project, project_dir) in projects {
            self.disk_reads.fetch_add(1, Ordering::Relaxed);
            if let Ok(meta) = read_session_meta(project_dir, sid) {
                let entry = CatalogEntry {
                    project: project.clone(),
                    project_dir: project_dir.clone(),
                    meta,
                };
                self.entries
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(sid.to_string(), entry.clone());
                return Some(entry);
            }
        }
        None
    }

    pub(crate) fn disk_reads(&self) -> u64 {
        self.disk_reads.load(Ordering::Relaxed)
    }
}
