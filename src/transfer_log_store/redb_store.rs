use std::{fmt, ops::Bound, path::Path};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use time::OffsetDateTime;

use crate::transfer_log_store::{
    LogPageToken, LogsPage, StoredBlockHeader, StoredTransferLog, StreamId, TransferLogCursor,
    TransferLogStreamConfig,
};

const META: TableDefinition<&[u8], &[u8]> = TableDefinition::new("transfer_log_meta_v1");
const HEADERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("transfer_log_headers_v1");
const LOGS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("transfer_log_logs_v1");
const RANGES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("transfer_log_ranges_v1");

const KEY_VERSION: u8 = 1;
const META_CONFIG: u8 = 1;
const META_CURSOR: u8 = 2;

#[derive(Debug, Error)]
pub enum RedbTransferLogStoreError {
    #[error("redb error: {0}")]
    Redb(String),

    #[error(transparent)]
    Serde(#[from] serde_json::Error),

    #[error("invalid limit {field}: must be greater than zero")]
    InvalidLimit { field: &'static str },
}

pub type RedbTransferLogStoreResult<T> = Result<T, RedbTransferLogStoreError>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeManifestDto {
    pub stream: StreamId,
    pub from_block: u64,
    pub to_block: u64,
    pub block_count: u64,
    pub log_count: usize,
    pub writer_epoch: u64,
    pub completed_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedbTransferLogBatch {
    pub stream: StreamId,
    pub headers: Vec<StoredBlockHeader>,
    pub logs: Vec<StoredTransferLog>,
    pub range_manifest: Option<RangeManifestDto>,
    pub cursor: TransferLogCursor,
}

pub struct RedbTransferLogStore {
    db: Database,
}

impl RedbTransferLogStore {
    pub fn open(path: impl AsRef<Path>) -> RedbTransferLogStoreResult<Self> {
        let db = Database::create(path).map_err(redb_error)?;
        Ok(Self { db })
    }

    pub fn save_stream_config(
        &self,
        config: &TransferLogStreamConfig,
    ) -> RedbTransferLogStoreResult<()> {
        let write = self.db.begin_write().map_err(redb_error)?;
        {
            let mut table = write.open_table(META).map_err(redb_error)?;
            let key = meta_key(config.stream_id(), META_CONFIG);
            let value = serialize(config)?;
            table
                .insert(key.as_slice(), value.as_slice())
                .map_err(redb_error)?;
        }
        write.commit().map_err(redb_error)
    }

    pub fn load_stream_config(
        &self,
        stream: StreamId,
    ) -> RedbTransferLogStoreResult<Option<TransferLogStreamConfig>> {
        self.load_meta(meta_key(stream, META_CONFIG))
    }

    pub fn save_cursor(&self, cursor: &TransferLogCursor) -> RedbTransferLogStoreResult<()> {
        let write = self.db.begin_write().map_err(redb_error)?;
        {
            let mut table = write.open_table(META).map_err(redb_error)?;
            let key = meta_key(cursor.stream, META_CURSOR);
            let value = serialize(cursor)?;
            table
                .insert(key.as_slice(), value.as_slice())
                .map_err(redb_error)?;
        }
        write.commit().map_err(redb_error)
    }

    pub fn load_cursor(
        &self,
        stream: StreamId,
    ) -> RedbTransferLogStoreResult<Option<TransferLogCursor>> {
        self.load_meta(meta_key(stream, META_CURSOR))
    }

    pub fn save_block_header(&self, header: &StoredBlockHeader) -> RedbTransferLogStoreResult<()> {
        let write = self.db.begin_write().map_err(redb_error)?;
        {
            let mut table = write.open_table(HEADERS).map_err(redb_error)?;
            let key = header_key(header.stream_id(), header.block_number);
            let value = serialize(header)?;
            table
                .insert(key.as_slice(), value.as_slice())
                .map_err(redb_error)?;
        }
        write.commit().map_err(redb_error)
    }

    pub fn load_block_header(
        &self,
        stream: StreamId,
        block: u64,
    ) -> RedbTransferLogStoreResult<Option<StoredBlockHeader>> {
        let read = self.db.begin_read().map_err(redb_error)?;
        let table = read.open_table(HEADERS).map_err(redb_error)?;
        let key = header_key(stream, block);
        table
            .get(key.as_slice())
            .map_err(redb_error)?
            .map(|value| deserialize(value.value()))
            .transpose()
    }

    pub fn upsert_transfer_log(&self, log: &StoredTransferLog) -> RedbTransferLogStoreResult<()> {
        let write = self.db.begin_write().map_err(redb_error)?;
        {
            let mut table = write.open_table(LOGS).map_err(redb_error)?;
            let key = log_key(log.stream_id(), log.block_number, log.log_index);
            let value = serialize(log)?;
            table
                .insert(key.as_slice(), value.as_slice())
                .map_err(redb_error)?;
        }
        write.commit().map_err(redb_error)
    }

    pub fn logs_page(
        &self,
        stream: StreamId,
        after: Option<LogPageToken>,
        limit: usize,
    ) -> RedbTransferLogStoreResult<LogsPage> {
        if limit == 0 {
            return Err(RedbTransferLogStoreError::InvalidLimit { field: "limit" });
        }

        let read = self.db.begin_read().map_err(redb_error)?;
        let table = read.open_table(LOGS).map_err(redb_error)?;
        let start = match after {
            Some(token) => log_key(stream, token.block_number, token.log_index),
            None => log_prefix(stream),
        };
        let end = prefix_end(&log_prefix(stream));
        let lower = if after.is_some() {
            Bound::Excluded(start.as_slice())
        } else {
            Bound::Included(start.as_slice())
        };
        let mut iter = table
            .range::<&[u8]>((lower, Bound::Excluded(end.as_slice())))
            .map_err(redb_error)?;

        let mut logs = Vec::with_capacity(limit);
        while logs.len() < limit {
            let Some(item) = iter.next() else {
                break;
            };
            let (_, value) = item.map_err(redb_error)?;
            logs.push(deserialize(value.value())?);
        }

        let next_log = match iter.next() {
            Some(item) => {
                let (_, value) = item.map_err(redb_error)?;
                Some(deserialize::<StoredTransferLog>(value.value())?)
            }
            None => None,
        };
        let next_token = logs.last().map(StoredTransferLog::page_token);
        let complete_to_block = complete_to_block(&logs, next_log.as_ref());

        Ok(LogsPage::new(stream, logs, next_token, complete_to_block))
    }

    pub fn write_batch(&self, batch: RedbTransferLogBatch) -> RedbTransferLogStoreResult<()> {
        let write = self.db.begin_write().map_err(redb_error)?;
        {
            let mut headers = write.open_table(HEADERS).map_err(redb_error)?;
            for header in &batch.headers {
                let key = header_key(batch.stream, header.block_number);
                let value = serialize(header)?;
                headers
                    .insert(key.as_slice(), value.as_slice())
                    .map_err(redb_error)?;
            }
        }
        {
            let mut logs = write.open_table(LOGS).map_err(redb_error)?;
            for log in &batch.logs {
                let key = log_key(batch.stream, log.block_number, log.log_index);
                let value = serialize(log)?;
                logs.insert(key.as_slice(), value.as_slice())
                    .map_err(redb_error)?;
            }
        }
        if let Some(manifest) = &batch.range_manifest {
            let mut ranges = write.open_table(RANGES).map_err(redb_error)?;
            let key = range_key(batch.stream, manifest.from_block, manifest.to_block);
            let value = serialize(manifest)?;
            ranges
                .insert(key.as_slice(), value.as_slice())
                .map_err(redb_error)?;
        }
        {
            let mut meta = write.open_table(META).map_err(redb_error)?;
            let key = meta_key(batch.stream, META_CURSOR);
            let value = serialize(&batch.cursor)?;
            meta.insert(key.as_slice(), value.as_slice())
                .map_err(redb_error)?;
        }
        write.commit().map_err(redb_error)
    }

    pub fn rewind_delete_from_block(
        &self,
        stream: StreamId,
        block: u64,
        now: OffsetDateTime,
    ) -> RedbTransferLogStoreResult<Option<TransferLogCursor>> {
        let write = self.db.begin_write().map_err(redb_error)?;
        let mut cursor = {
            let meta = write.open_table(META).map_err(redb_error)?;
            let key = meta_key(stream, META_CURSOR);
            meta.get(key.as_slice())
                .map_err(redb_error)?
                .map(|value| deserialize::<TransferLogCursor>(value.value()))
                .transpose()?
        };

        {
            let mut headers = write.open_table(HEADERS).map_err(redb_error)?;
            remove_from_prefix_range(
                &mut headers,
                &header_key(stream, block),
                &header_prefix(stream),
            )?;
        }
        {
            let mut logs = write.open_table(LOGS).map_err(redb_error)?;
            remove_from_prefix_range(&mut logs, &log_key(stream, block, 0), &log_prefix(stream))?;
        }
        {
            let mut ranges = write.open_table(RANGES).map_err(redb_error)?;
            let keys = range_keys_with_to_at_or_after(&ranges, stream, block)?;
            for key in keys {
                ranges.remove(key.as_slice()).map_err(redb_error)?;
            }
        }
        if let Some(cursor) = &mut cursor {
            cursor.record_rewind(block, cursor.writer_epoch, now);
            let mut meta = write.open_table(META).map_err(redb_error)?;
            let key = meta_key(stream, META_CURSOR);
            let value = serialize(cursor)?;
            meta.insert(key.as_slice(), value.as_slice())
                .map_err(redb_error)?;
        }

        write.commit().map_err(redb_error)?;
        Ok(cursor)
    }

    pub fn range_manifests(
        &self,
        stream: StreamId,
        limit: usize,
    ) -> RedbTransferLogStoreResult<Vec<RangeManifestDto>> {
        if limit == 0 {
            return Err(RedbTransferLogStoreError::InvalidLimit { field: "limit" });
        }

        let read = self.db.begin_read().map_err(redb_error)?;
        let table = read.open_table(RANGES).map_err(redb_error)?;
        let prefix = range_prefix(stream);
        let end = prefix_end(&prefix);
        let mut manifests = Vec::new();
        for item in table
            .range::<&[u8]>((
                Bound::Included(prefix.as_slice()),
                Bound::Excluded(end.as_slice()),
            ))
            .map_err(redb_error)?
            .take(limit)
        {
            let (_, value) = item.map_err(redb_error)?;
            manifests.push(deserialize(value.value())?);
        }
        Ok(manifests)
    }

    fn load_meta<T: DeserializeOwned>(
        &self,
        key: Vec<u8>,
    ) -> RedbTransferLogStoreResult<Option<T>> {
        let read = self.db.begin_read().map_err(redb_error)?;
        let table = read.open_table(META).map_err(redb_error)?;
        table
            .get(key.as_slice())
            .map_err(redb_error)?
            .map(|value| deserialize(value.value()))
            .transpose()
    }
}

fn remove_from_prefix_range(
    table: &mut redb::Table<'_, &[u8], &[u8]>,
    start: &[u8],
    prefix: &[u8],
) -> RedbTransferLogStoreResult<()> {
    let end = prefix_end(prefix);
    let keys = table
        .range::<&[u8]>((Bound::Included(start), Bound::Excluded(end.as_slice())))
        .map_err(redb_error)?
        .map(|item| item.map(|(key, _)| key.value().to_vec()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(redb_error)?;
    for key in keys {
        table.remove(key.as_slice()).map_err(redb_error)?;
    }
    Ok(())
}

fn range_keys_with_to_at_or_after(
    table: &redb::Table<'_, &[u8], &[u8]>,
    stream: StreamId,
    block: u64,
) -> RedbTransferLogStoreResult<Vec<Vec<u8>>> {
    let prefix = range_prefix(stream);
    let end = prefix_end(&prefix);
    let mut keys = Vec::new();
    for item in table
        .range::<&[u8]>((
            Bound::Included(prefix.as_slice()),
            Bound::Excluded(end.as_slice()),
        ))
        .map_err(redb_error)?
    {
        let (key, value) = item.map_err(redb_error)?;
        let manifest: RangeManifestDto = deserialize(value.value())?;
        if manifest.to_block >= block {
            keys.push(key.value().to_vec());
        }
    }
    Ok(keys)
}

fn complete_to_block(
    logs: &[StoredTransferLog],
    next_log: Option<&StoredTransferLog>,
) -> Option<u64> {
    let last = logs.last()?;
    match next_log {
        Some(next) if next.block_number == last.block_number => last.block_number.checked_sub(1),
        _ => Some(last.block_number),
    }
}

fn serialize<T: Serialize>(value: &T) -> RedbTransferLogStoreResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(Into::into)
}

fn deserialize<T: DeserializeOwned>(bytes: &[u8]) -> RedbTransferLogStoreResult<T> {
    serde_json::from_slice(bytes).map_err(Into::into)
}

fn redb_error(error: impl fmt::Display) -> RedbTransferLogStoreError {
    RedbTransferLogStoreError::Redb(error.to_string())
}

fn meta_key(stream: StreamId, kind: u8) -> Vec<u8> {
    let mut key = stream_key(stream, b"m");
    key.push(kind);
    key
}

fn header_prefix(stream: StreamId) -> Vec<u8> {
    stream_key(stream, b"h")
}

fn header_key(stream: StreamId, block: u64) -> Vec<u8> {
    let mut key = header_prefix(stream);
    key.extend_from_slice(&block.to_be_bytes());
    key
}

fn log_prefix(stream: StreamId) -> Vec<u8> {
    stream_key(stream, b"l")
}

fn log_key(stream: StreamId, block: u64, log_index: u64) -> Vec<u8> {
    let mut key = log_prefix(stream);
    key.extend_from_slice(&block.to_be_bytes());
    key.extend_from_slice(&log_index.to_be_bytes());
    key
}

fn range_prefix(stream: StreamId) -> Vec<u8> {
    stream_key(stream, b"r")
}

fn range_key(stream: StreamId, from_block: u64, to_block: u64) -> Vec<u8> {
    let mut key = range_prefix(stream);
    key.extend_from_slice(&from_block.to_be_bytes());
    key.extend_from_slice(&to_block.to_be_bytes());
    key
}

fn stream_key(stream: StreamId, namespace: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + namespace.len() + 8 + 20);
    key.push(KEY_VERSION);
    key.extend_from_slice(namespace);
    key.extend_from_slice(&stream.chain_id.to_be_bytes());
    key.extend_from_slice(stream.token_address.as_bytes());
    key
}

fn prefix_end(prefix: &[u8]) -> Vec<u8> {
    let mut end = prefix.to_vec();
    for index in (0..end.len()).rev() {
        if end[index] != u8::MAX {
            end[index] += 1;
            end.truncate(index + 1);
            return end;
        }
    }
    Vec::new()
}
