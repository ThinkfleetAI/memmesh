// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("schema mismatch: {0}")]
    SchemaMismatch(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("unsupported on this backend: {0}")]
    Unsupported(String),

    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
