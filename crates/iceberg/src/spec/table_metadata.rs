// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

//! Defines the [table metadata](https://iceberg.apache.org/spec/#table-metadata).
//! The main struct here is [TableMetadataV2] which defines the data for a table.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::hash::Hash;
use std::sync::Arc;

use _serde::TableMetadataEnum;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use uuid::Uuid;

use super::snapshot::SnapshotReference;
pub use super::table_metadata_builder::{TableMetadataBuildResult, TableMetadataBuilder};
use super::{
    DEFAULT_PARTITION_SPEC_ID, PartitionSpecRef, PartitionStatisticsFile, SchemaId, SchemaRef,
    SnapshotRef, SnapshotRetention, SortOrder, SortOrderRef, StatisticsFile, StructType,
};
use crate::error::{Result, timestamp_ms_to_utc};
use crate::io::FileIO;
use crate::{Error, ErrorKind};

static MAIN_BRANCH: &str = "main";
pub(crate) static ONE_MINUTE_MS: i64 = 60_000;

pub(crate) static EMPTY_SNAPSHOT_ID: i64 = -1;
pub(crate) static INITIAL_SEQUENCE_NUMBER: i64 = 0;

/// Reserved table property for table format version.
///
/// Iceberg will default a new table's format version to the latest stable and recommended
/// version. This reserved property keyword allows users to override the Iceberg format version of
/// the table metadata.
///
/// If this table property exists when creating a table, the table will use the specified format
/// version. If a table updates this property, it will try to upgrade to the specified format
/// version.
pub const PROPERTY_FORMAT_VERSION: &str = "format-version";
/// Reserved table property for table UUID.
pub const PROPERTY_UUID: &str = "uuid";
/// Reserved table property for the total number of snapshots.
pub const PROPERTY_SNAPSHOT_COUNT: &str = "snapshot-count";
/// Reserved table property for current snapshot summary.
pub const PROPERTY_CURRENT_SNAPSHOT_SUMMARY: &str = "current-snapshot-summary";
/// Reserved table property for current snapshot id.
pub const PROPERTY_CURRENT_SNAPSHOT_ID: &str = "current-snapshot-id";
/// Reserved table property for current snapshot timestamp.
pub const PROPERTY_CURRENT_SNAPSHOT_TIMESTAMP: &str = "current-snapshot-timestamp-ms";
/// Reserved table property for the JSON representation of current schema.
pub const PROPERTY_CURRENT_SCHEMA: &str = "current-schema";
/// Reserved table property for the JSON representation of current(default) partition spec.
pub const PROPERTY_DEFAULT_PARTITION_SPEC: &str = "default-partition-spec";
/// Reserved table property for the JSON representation of current(default) sort order.
pub const PROPERTY_DEFAULT_SORT_ORDER: &str = "default-sort-order";

/// Property key for max number of previous versions to keep.
pub const PROPERTY_METADATA_PREVIOUS_VERSIONS_MAX: &str = "write.metadata.previous-versions-max";
/// Default value for max number of previous versions to keep.
pub const PROPERTY_METADATA_PREVIOUS_VERSIONS_MAX_DEFAULT: usize = 100;

/// Property key for max number of partitions to keep summary stats for.
pub const PROPERTY_WRITE_PARTITION_SUMMARY_LIMIT: &str = "write.summary.partition-limit";
/// Default value for the max number of partitions to keep summary stats for.
pub const PROPERTY_WRITE_PARTITION_SUMMARY_LIMIT_DEFAULT: u64 = 0;

/// Reserved Iceberg table properties list.
///
/// Reserved table properties are only used to control behaviors when creating or updating a
/// table. The value of these properties are not persisted as a part of the table metadata.
pub const RESERVED_PROPERTIES: [&str; 9] = [
    PROPERTY_FORMAT_VERSION,
    PROPERTY_UUID,
    PROPERTY_SNAPSHOT_COUNT,
    PROPERTY_CURRENT_SNAPSHOT_ID,
    PROPERTY_CURRENT_SNAPSHOT_SUMMARY,
    PROPERTY_CURRENT_SNAPSHOT_TIMESTAMP,
    PROPERTY_CURRENT_SCHEMA,
    PROPERTY_DEFAULT_PARTITION_SPEC,
    PROPERTY_DEFAULT_SORT_ORDER,
];

/// Property key for number of commit retries.
pub const PROPERTY_COMMIT_NUM_RETRIES: &str = "commit.retry.num-retries";
/// Default value for number of commit retries.
pub const PROPERTY_COMMIT_NUM_RETRIES_DEFAULT: usize = 4;

/// Property key for minimum wait time (ms) between retries.
pub const PROPERTY_COMMIT_MIN_RETRY_WAIT_MS: &str = "commit.retry.min-wait-ms";
/// Default value for minimum wait time (ms) between retries.
pub const PROPERTY_COMMIT_MIN_RETRY_WAIT_MS_DEFAULT: u64 = 100;

/// Property key for maximum wait time (ms) between retries.
pub const PROPERTY_COMMIT_MAX_RETRY_WAIT_MS: &str = "commit.retry.max-wait-ms";
/// Default value for maximum wait time (ms) between retries.
pub const PROPERTY_COMMIT_MAX_RETRY_WAIT_MS_DEFAULT: u64 = 60 * 1000; // 1 minute

/// Property key for total maximum retry time (ms).
pub const PROPERTY_COMMIT_TOTAL_RETRY_TIME_MS: &str = "commit.retry.total-timeout-ms";
/// Default value for total maximum retry time (ms).
pub const PROPERTY_COMMIT_TOTAL_RETRY_TIME_MS_DEFAULT: u64 = 30 * 60 * 1000; // 30 minutes

/// Reference to [`TableMetadata`].
pub type TableMetadataRef = Arc<TableMetadata>;

#[derive(Debug, PartialEq, Deserialize, Eq, Clone)]
#[serde(try_from = "TableMetadataEnum")]
/// Fields for the version 2 of the table metadata.
///
/// We assume that this data structure is always valid, so we will panic when invalid error happens.
/// We check the validity of this data structure when constructing.
pub struct TableMetadata {
    /// Integer Version for the format.
    pub(crate) format_version: FormatVersion,
    /// A UUID that identifies the table
    pub(crate) table_uuid: Uuid,
    /// Location tables base location
    pub(crate) location: String,
    /// The tables highest sequence number
    pub(crate) last_sequence_number: i64,
    /// Timestamp in milliseconds from the unix epoch when the table was last updated.
    pub(crate) last_updated_ms: i64,
    /// An integer; the highest assigned column ID for the table.
    pub(crate) last_column_id: i32,
    /// A list of schemas, stored as objects with schema-id.
    pub(crate) schemas: HashMap<i32, SchemaRef>,
    /// ID of the table’s current schema.
    pub(crate) current_schema_id: i32,
    /// A list of partition specs, stored as full partition spec objects.
    pub(crate) partition_specs: HashMap<i32, PartitionSpecRef>,
    /// ID of the “current” spec that writers should use by default.
    pub(crate) default_spec: PartitionSpecRef,
    /// Partition type of the default partition spec.
    pub(crate) default_partition_type: StructType,
    /// An integer; the highest assigned partition field ID across all partition specs for the table.
    pub(crate) last_partition_id: i32,
    ///A string to string map of table properties. This is used to control settings that
    /// affect reading and writing and is not intended to be used for arbitrary metadata.
    /// For example, commit.retry.num-retries is used to control the number of commit retries.
    pub(crate) properties: HashMap<String, String>,
    /// long ID of the current table snapshot; must be the same as the current
    /// ID of the main branch in refs.
    pub(crate) current_snapshot_id: Option<i64>,
    ///A list of valid snapshots. Valid snapshots are snapshots for which all
    /// data files exist in the file system. A data file must not be deleted
    /// from the file system until the last snapshot in which it was listed is
    /// garbage collected.
    pub(crate) snapshots: HashMap<i64, SnapshotRef>,
    /// A list (optional) of timestamp and snapshot ID pairs that encodes changes
    /// to the current snapshot for the table. Each time the current-snapshot-id
    /// is changed, a new entry should be added with the last-updated-ms
    /// and the new current-snapshot-id. When snapshots are expired from
    /// the list of valid snapshots, all entries before a snapshot that has
    /// expired should be removed.
    pub(crate) snapshot_log: Vec<SnapshotLog>,

    /// A list (optional) of timestamp and metadata file location pairs
    /// that encodes changes to the previous metadata files for the table.
    /// Each time a new metadata file is created, a new entry of the
    /// previous metadata file location should be added to the list.
    /// Tables can be configured to remove the oldest metadata log entries and
    /// keep a fixed-size log of the most recent entries after a commit.
    pub(crate) metadata_log: Vec<MetadataLog>,

    /// A list of sort orders, stored as full sort order objects.
    pub(crate) sort_orders: HashMap<i64, SortOrderRef>,
    /// Default sort order id of the table. Note that this could be used by
    /// writers, but is not used when reading because reads use the specs
    /// stored in manifest files.
    pub(crate) default_sort_order_id: i64,
    /// A map of snapshot references. The map keys are the unique snapshot reference
    /// names in the table, and the map values are snapshot reference objects.
    /// There is always a main branch reference pointing to the current-snapshot-id
    /// even if the refs map is null.
    pub(crate) refs: HashMap<String, SnapshotReference>,
    /// Mapping of snapshot ids to statistics files.
    pub(crate) statistics: HashMap<i64, StatisticsFile>,
    /// Mapping of snapshot ids to partition statistics files.
    pub(crate) partition_statistics: HashMap<i64, PartitionStatisticsFile>,
    /// Encryption Keys
    pub(crate) encryption_keys: HashMap<String, String>,
}

impl TableMetadata {
    /// Convert this Table Metadata into a builder for modification.
    ///
    /// `current_file_location` is the location where the current version
    /// of the metadata file is stored. This is used to update the metadata log.
    /// If `current_file_location` is `None`, the metadata log will not be updated.
    /// This should only be used to stage-create tables.
    #[must_use]
    pub fn into_builder(self, current_file_location: Option<String>) -> TableMetadataBuilder {
        TableMetadataBuilder::new_from_metadata(self, current_file_location)
    }

    /// Returns format version of this metadata.
    #[inline]
    pub fn format_version(&self) -> FormatVersion {
        self.format_version
    }

    /// Returns uuid of current table.
    #[inline]
    pub fn uuid(&self) -> Uuid {
        self.table_uuid
    }

    /// Returns table location.
    #[inline]
    pub fn location(&self) -> &str {
        self.location.as_str()
    }

    /// Returns last sequence number.
    #[inline]
    pub fn last_sequence_number(&self) -> i64 {
        self.last_sequence_number
    }

    /// Returns the next sequence number for the table.
    ///
    /// For format version 1, it always returns the initial sequence number.
    /// For other versions, it returns the last sequence number incremented by 1.
    #[inline]
    pub fn next_sequence_number(&self) -> i64 {
        match self.format_version {
            FormatVersion::V1 => INITIAL_SEQUENCE_NUMBER,
            _ => self.last_sequence_number + 1,
        }
    }

    /// Returns the last column id.
    #[inline]
    pub fn last_column_id(&self) -> i32 {
        self.last_column_id
    }

    /// Returns the last partition_id
    #[inline]
    pub fn last_partition_id(&self) -> i32 {
        self.last_partition_id
    }

    /// Returns last updated time.
    #[inline]
    pub fn last_updated_timestamp(&self) -> Result<DateTime<Utc>> {
        timestamp_ms_to_utc(self.last_updated_ms)
    }

    /// Returns last updated time in milliseconds.
    #[inline]
    pub fn last_updated_ms(&self) -> i64 {
        self.last_updated_ms
    }

    /// Returns schemas
    #[inline]
    pub fn schemas_iter(&self) -> impl ExactSizeIterator<Item = &SchemaRef> {
        self.schemas.values()
    }

    /// Lookup schema by id.
    #[inline]
    pub fn schema_by_id(&self, schema_id: SchemaId) -> Option<&SchemaRef> {
        self.schemas.get(&schema_id)
    }

    /// Get current schema
    #[inline]
    pub fn current_schema(&self) -> &SchemaRef {
        self.schema_by_id(self.current_schema_id)
            .expect("Current schema id set, but not found in table metadata")
    }

    /// Get the id of the current schema
    #[inline]
    pub fn current_schema_id(&self) -> SchemaId {
        self.current_schema_id
    }

    /// Returns all partition specs.
    #[inline]
    pub fn partition_specs_iter(&self) -> impl ExactSizeIterator<Item = &PartitionSpecRef> {
        self.partition_specs.values()
    }

    /// Lookup partition spec by id.
    #[inline]
    pub fn partition_spec_by_id(&self, spec_id: i32) -> Option<&PartitionSpecRef> {
        self.partition_specs.get(&spec_id)
    }

    /// Get default partition spec
    #[inline]
    pub fn default_partition_spec(&self) -> &PartitionSpecRef {
        &self.default_spec
    }

    /// Return the partition type of the default partition spec.
    #[inline]
    pub fn default_partition_type(&self) -> &StructType {
        &self.default_partition_type
    }

    #[inline]
    /// Returns spec id of the "current" partition spec.
    pub fn default_partition_spec_id(&self) -> i32 {
        self.default_spec.spec_id()
    }

    /// Returns all snapshots
    #[inline]
    pub fn snapshots(&self) -> impl ExactSizeIterator<Item = &SnapshotRef> {
        self.snapshots.values()
    }

    /// Lookup snapshot by id.
    #[inline]
    pub fn snapshot_by_id(&self, snapshot_id: i64) -> Option<&SnapshotRef> {
        self.snapshots.get(&snapshot_id)
    }

    /// Returns snapshot history.
    #[inline]
    pub fn history(&self) -> &[SnapshotLog] {
        &self.snapshot_log
    }

    /// Returns the metadata log.
    #[inline]
    pub fn metadata_log(&self) -> &[MetadataLog] {
        &self.metadata_log
    }

    /// Get current snapshot
    #[inline]
    pub fn current_snapshot(&self) -> Option<&SnapshotRef> {
        self.current_snapshot_id.map(|s| {
            self.snapshot_by_id(s)
                .expect("Current snapshot id has been set, but doesn't exist in metadata")
        })
    }

    /// Get the current snapshot id
    #[inline]
    pub fn current_snapshot_id(&self) -> Option<i64> {
        self.current_snapshot_id
    }

    /// Get the snapshot for a reference
    /// Returns an option if the `ref_name` is not found
    #[inline]
    pub fn snapshot_for_ref(&self, ref_name: &str) -> Option<&SnapshotRef> {
        self.refs.get(ref_name).map(|r| {
            self.snapshot_by_id(r.snapshot_id)
                .unwrap_or_else(|| panic!("Snapshot id of ref {} doesn't exist", ref_name))
        })
    }

    /// Return all sort orders.
    #[inline]
    pub fn sort_orders_iter(&self) -> impl ExactSizeIterator<Item = &SortOrderRef> {
        self.sort_orders.values()
    }

    /// Lookup sort order by id.
    #[inline]
    pub fn sort_order_by_id(&self, sort_order_id: i64) -> Option<&SortOrderRef> {
        self.sort_orders.get(&sort_order_id)
    }

    /// Returns default sort order id.
    #[inline]
    pub fn default_sort_order(&self) -> &SortOrderRef {
        self.sort_orders
            .get(&self.default_sort_order_id)
            .expect("Default order id has been set, but not found in table metadata!")
    }

    /// Returns default sort order id.
    #[inline]
    pub fn default_sort_order_id(&self) -> i64 {
        self.default_sort_order_id
    }

    /// Returns properties of table.
    #[inline]
    pub fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    /// Return location of statistics files.
    #[inline]
    pub fn statistics_iter(&self) -> impl ExactSizeIterator<Item = &StatisticsFile> {
        self.statistics.values()
    }

    /// Return location of partition statistics files.
    #[inline]
    pub fn partition_statistics_iter(
        &self,
    ) -> impl ExactSizeIterator<Item = &PartitionStatisticsFile> {
        self.partition_statistics.values()
    }

    /// Get a statistics file for a snapshot id.
    #[inline]
    pub fn statistics_for_snapshot(&self, snapshot_id: i64) -> Option<&StatisticsFile> {
        self.statistics.get(&snapshot_id)
    }

    /// Get a partition statistics file for a snapshot id.
    #[inline]
    pub fn partition_statistics_for_snapshot(
        &self,
        snapshot_id: i64,
    ) -> Option<&PartitionStatisticsFile> {
        self.partition_statistics.get(&snapshot_id)
    }

    fn construct_refs(&mut self) {
        if let Some(current_snapshot_id) = self.current_snapshot_id {
            if !self.refs.contains_key(MAIN_BRANCH) {
                self.refs
                    .insert(MAIN_BRANCH.to_string(), SnapshotReference {
                        snapshot_id: current_snapshot_id,
                        retention: SnapshotRetention::Branch {
                            min_snapshots_to_keep: None,
                            max_snapshot_age_ms: None,
                            max_ref_age_ms: None,
                        },
                    });
            }
        }
    }

    /// Iterate over all encryption keys
    #[inline]
    pub fn encryption_keys_iter(&self) -> impl ExactSizeIterator<Item = (&String, &String)> {
        self.encryption_keys.iter()
    }

    /// Get the encryption key for a given key id
    #[inline]
    pub fn encryption_key(&self, key_id: &str) -> Option<&String> {
        self.encryption_keys.get(key_id)
    }

    /// Read table metadata from the given location.
    pub async fn read_from(
        file_io: &FileIO,
        metadata_location: impl AsRef<str>,
    ) -> Result<TableMetadata> {
        let input_file = file_io.new_input(metadata_location)?;
        let metadata_content = input_file.read().await?;
        let metadata = serde_json::from_slice::<TableMetadata>(&metadata_content)?;
        Ok(metadata)
    }

    /// Write table metadata to the given location.
    pub async fn write_to(
        &self,
        file_io: &FileIO,
        metadata_location: impl AsRef<str>,
    ) -> Result<()> {
        file_io
            .new_output(metadata_location)?
            .write(serde_json::to_vec(self)?.into())
            .await
    }

    /// Normalize this partition spec.
    ///
    /// This is an internal method
    /// meant to be called after constructing table metadata from untrusted sources.
    /// We run this method after json deserialization.
    /// All constructors for `TableMetadata` which are part of `iceberg-rust`
    /// should return normalized `TableMetadata`.
    pub(super) fn try_normalize(&mut self) -> Result<&mut Self> {
        self.validate_current_schema()?;
        self.normalize_current_snapshot()?;
        self.construct_refs();
        self.validate_refs()?;
        self.validate_chronological_snapshot_logs()?;
        self.validate_chronological_metadata_logs()?;
        // Normalize location (remove trailing slash)
        self.location = self.location.trim_end_matches('/').to_string();
        self.validate_snapshot_sequence_number()?;
        self.try_normalize_partition_spec()?;
        self.try_normalize_sort_order()?;
        Ok(self)
    }

    /// If the default partition spec is not present in specs, add it
    fn try_normalize_partition_spec(&mut self) -> Result<()> {
        if self
            .partition_spec_by_id(self.default_spec.spec_id())
            .is_none()
        {
            self.partition_specs.insert(
                self.default_spec.spec_id(),
                Arc::new(Arc::unwrap_or_clone(self.default_spec.clone())),
            );
        }

        Ok(())
    }

    /// If the default sort order is unsorted but the sort order is not present, add it
    fn try_normalize_sort_order(&mut self) -> Result<()> {
        if self.sort_order_by_id(self.default_sort_order_id).is_some() {
            return Ok(());
        }

        if self.default_sort_order_id != SortOrder::UNSORTED_ORDER_ID {
            return Err(Error::new(
                ErrorKind::DataInvalid,
                format!(
                    "No sort order exists with the default sort order id {}.",
                    self.default_sort_order_id
                ),
            ));
        }

        let sort_order = SortOrder::unsorted_order();
        self.sort_orders
            .insert(SortOrder::UNSORTED_ORDER_ID, Arc::new(sort_order));
        Ok(())
    }

    /// Validate the current schema is set and exists.
    fn validate_current_schema(&self) -> Result<()> {
        if self.schema_by_id(self.current_schema_id).is_none() {
            return Err(Error::new(
                ErrorKind::DataInvalid,
                format!(
                    "No schema exists with the current schema id {}.",
                    self.current_schema_id
                ),
            ));
        }
        Ok(())
    }

    /// If current snapshot is Some(-1) then set it to None.
    fn normalize_current_snapshot(&mut self) -> Result<()> {
        if let Some(current_snapshot_id) = self.current_snapshot_id {
            if current_snapshot_id == EMPTY_SNAPSHOT_ID {
                self.current_snapshot_id = None;
            } else if self.snapshot_by_id(current_snapshot_id).is_none() {
                return Err(Error::new(
                    ErrorKind::DataInvalid,
                    format!(
                        "Snapshot for current snapshot id {} does not exist in the existing snapshots list",
                        current_snapshot_id
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Validate that all refs are valid (snapshot exists)
    fn validate_refs(&self) -> Result<()> {
        for (name, snapshot_ref) in self.refs.iter() {
            if self.snapshot_by_id(snapshot_ref.snapshot_id).is_none() {
                return Err(Error::new(
                    ErrorKind::DataInvalid,
                    format!(
                        "Snapshot for reference {name} does not exist in the existing snapshots list"
                    ),
                ));
            }
        }

        let main_ref = self.refs.get(MAIN_BRANCH);
        if self.current_snapshot_id.is_some() {
            if let Some(main_ref) = main_ref {
                if main_ref.snapshot_id != self.current_snapshot_id.unwrap_or_default() {
                    return Err(Error::new(
                        ErrorKind::DataInvalid,
                        format!(
                            "Current snapshot id does not match main branch ({:?} != {:?})",
                            self.current_snapshot_id.unwrap_or_default(),
                            main_ref.snapshot_id
                        ),
                    ));
                }
            }
        } else if main_ref.is_some() {
            return Err(Error::new(
                ErrorKind::DataInvalid,
                "Current snapshot is not set, but main branch exists",
            ));
        }

        Ok(())
    }

    /// Validate that for V1 Metadata the last_sequence_number is 0
    fn validate_snapshot_sequence_number(&self) -> Result<()> {
        if self.format_version < FormatVersion::V2 && self.last_sequence_number != 0 {
            return Err(Error::new(
                ErrorKind::DataInvalid,
                format!(
                    "Last sequence number must be 0 in v1. Found {}",
                    self.last_sequence_number
                ),
            ));
        }

        if self.format_version >= FormatVersion::V2 {
            if let Some(snapshot) = self
                .snapshots
                .values()
                .find(|snapshot| snapshot.sequence_number() > self.last_sequence_number)
            {
                return Err(Error::new(
                    ErrorKind::DataInvalid,
                    format!(
                        "Invalid snapshot with id {} and sequence number {} greater than last sequence number {}",
                        snapshot.snapshot_id(),
                        snapshot.sequence_number(),
                        self.last_sequence_number
                    ),
                ));
            }
        }

        Ok(())
    }

    /// Validate snapshots logs are chronological and last updated is after the last snapshot log.
    fn validate_chronological_snapshot_logs(&self) -> Result<()> {
        for window in self.snapshot_log.windows(2) {
            let (prev, curr) = (&window[0], &window[1]);
            // commits can happen concurrently from different machines.
            // A tolerance helps us avoid failure for small clock skew
            if curr.timestamp_ms - prev.timestamp_ms < -ONE_MINUTE_MS {
                return Err(Error::new(
                    ErrorKind::DataInvalid,
                    "Expected sorted snapshot log entries",
                ));
            }
        }

        if let Some(last) = self.snapshot_log.last() {
            // commits can happen concurrently from different machines.
            // A tolerance helps us avoid failure for small clock skew
            if self.last_updated_ms - last.timestamp_ms < -ONE_MINUTE_MS {
                return Err(Error::new(
                    ErrorKind::DataInvalid,
                    format!(
                        "Invalid update timestamp {}: before last snapshot log entry at {}",
                        self.last_updated_ms, last.timestamp_ms
                    ),
                ));
            }
        }
        Ok(())
    }

    fn validate_chronological_metadata_logs(&self) -> Result<()> {
        for window in self.metadata_log.windows(2) {
            let (prev, curr) = (&window[0], &window[1]);
            // commits can happen concurrently from different machines.
            // A tolerance helps us avoid failure for small clock skew
            if curr.timestamp_ms - prev.timestamp_ms < -ONE_MINUTE_MS {
                return Err(Error::new(
                    ErrorKind::DataInvalid,
                    "Expected sorted metadata log entries",
                ));
            }
        }

        if let Some(last) = self.metadata_log.last() {
            // commits can happen concurrently from different machines.
            // A tolerance helps us avoid failure for small clock skew
            if self.last_updated_ms - last.timestamp_ms < -ONE_MINUTE_MS {
                return Err(Error::new(
                    ErrorKind::DataInvalid,
                    format!(
                        "Invalid update timestamp {}: before last metadata log entry at {}",
                        self.last_updated_ms, last.timestamp_ms
                    ),
                ));
            }
        }

        Ok(())
    }
}

pub(super) mod _serde {
    use std::borrow::BorrowMut;
    /// This is a helper module that defines types to help with serialization/deserialization.
    /// For deserialization the input first gets read into either the [TableMetadataV1] or [TableMetadataV2] struct
    /// and then converted into the [TableMetadata] struct. Serialization works the other way around.
    /// [TableMetadataV1] and [TableMetadataV2] are internal struct that are only used for serialization and deserialization.
    use std::collections::HashMap;
    /// This is a helper module that defines types to help with serialization/deserialization.
    /// For deserialization the input first gets read into either the [TableMetadataV1] or [TableMetadataV2] struct
    /// and then converted into the [TableMetadata] struct. Serialization works the other way around.
    /// [TableMetadataV1] and [TableMetadataV2] are internal struct that are only used for serialization and deserialization.
    use std::sync::Arc;

    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    use super::{
        DEFAULT_PARTITION_SPEC_ID, FormatVersion, MAIN_BRANCH, MetadataLog, SnapshotLog,
        TableMetadata,
    };
    use crate::spec::schema::_serde::{SchemaV1, SchemaV2};
    use crate::spec::snapshot::_serde::{SnapshotV1, SnapshotV2};
    use crate::spec::{
        PartitionField, PartitionSpec, PartitionSpecRef, PartitionStatisticsFile, Schema,
        SchemaRef, Snapshot, SnapshotReference, SnapshotRetention, SortOrder, StatisticsFile,
    };
    use crate::{Error, ErrorKind};

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(untagged)]
    pub(super) enum TableMetadataEnum {
        V2(TableMetadataV2),
        V1(TableMetadataV1),
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "kebab-case")]
    /// Defines the structure of a v2 table metadata for serialization/deserialization
    pub(super) struct TableMetadataV2 {
        pub format_version: VersionNumber<2>,
        pub table_uuid: Uuid,
        pub location: String,
        pub last_sequence_number: i64,
        pub last_updated_ms: i64,
        pub last_column_id: i32,
        pub schemas: Vec<SchemaV2>,
        pub current_schema_id: i32,
        pub partition_specs: Vec<PartitionSpec>,
        pub default_spec_id: i32,
        pub last_partition_id: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub properties: Option<HashMap<String, String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub current_snapshot_id: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub snapshots: Option<Vec<SnapshotV2>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub snapshot_log: Option<Vec<SnapshotLog>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub metadata_log: Option<Vec<MetadataLog>>,
        pub sort_orders: Vec<SortOrder>,
        pub default_sort_order_id: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub refs: Option<HashMap<String, SnapshotReference>>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub statistics: Vec<StatisticsFile>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub partition_statistics: Vec<PartitionStatisticsFile>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "kebab-case")]
    /// Defines the structure of a v1 table metadata for serialization/deserialization
    pub(super) struct TableMetadataV1 {
        pub format_version: VersionNumber<1>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub table_uuid: Option<Uuid>,
        pub location: String,
        pub last_updated_ms: i64,
        pub last_column_id: i32,
        /// `schema` is optional to prioritize `schemas` and `current-schema-id`, allowing liberal reading of V1 metadata.
        pub schema: Option<SchemaV1>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub schemas: Option<Vec<SchemaV1>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub current_schema_id: Option<i32>,
        /// `partition_spec` is optional to prioritize `partition_specs`, aligning with liberal reading of potentially invalid V1 metadata.
        pub partition_spec: Option<Vec<PartitionField>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub partition_specs: Option<Vec<PartitionSpec>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub default_spec_id: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub last_partition_id: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub properties: Option<HashMap<String, String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub current_snapshot_id: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub snapshots: Option<Vec<SnapshotV1>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub snapshot_log: Option<Vec<SnapshotLog>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub metadata_log: Option<Vec<MetadataLog>>,
        pub sort_orders: Option<Vec<SortOrder>>,
        pub default_sort_order_id: Option<i64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub statistics: Vec<StatisticsFile>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub partition_statistics: Vec<PartitionStatisticsFile>,
    }

    /// Helper to serialize and deserialize the format version.
    #[derive(Debug, PartialEq, Eq)]
    pub(crate) struct VersionNumber<const V: u8>;

    impl Serialize for TableMetadata {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where S: serde::Serializer {
            // we must do a clone here
            let table_metadata_enum: TableMetadataEnum =
                self.clone().try_into().map_err(serde::ser::Error::custom)?;

            table_metadata_enum.serialize(serializer)
        }
    }

    impl<const V: u8> Serialize for VersionNumber<V> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where S: serde::Serializer {
            serializer.serialize_u8(V)
        }
    }

    impl<'de, const V: u8> Deserialize<'de> for VersionNumber<V> {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where D: serde::Deserializer<'de> {
            let value = u8::deserialize(deserializer)?;
            if value == V {
                Ok(VersionNumber::<V>)
            } else {
                Err(serde::de::Error::custom("Invalid Version"))
            }
        }
    }

    impl TryFrom<TableMetadataEnum> for TableMetadata {
        type Error = Error;
        fn try_from(value: TableMetadataEnum) -> Result<Self, Error> {
            match value {
                TableMetadataEnum::V2(value) => value.try_into(),
                TableMetadataEnum::V1(value) => value.try_into(),
            }
        }
    }

    impl TryFrom<TableMetadata> for TableMetadataEnum {
        type Error = Error;
        fn try_from(value: TableMetadata) -> Result<Self, Error> {
            Ok(match value.format_version {
                FormatVersion::V2 => TableMetadataEnum::V2(value.into()),
                FormatVersion::V1 => TableMetadataEnum::V1(value.try_into()?),
            })
        }
    }

    impl TryFrom<TableMetadataV2> for TableMetadata {
        type Error = Error;
        fn try_from(value: TableMetadataV2) -> Result<Self, self::Error> {
            let current_snapshot_id = if let &Some(-1) = &value.current_snapshot_id {
                None
            } else {
                value.current_snapshot_id
            };
            let schemas = HashMap::from_iter(
                value
                    .schemas
                    .into_iter()
                    .map(|schema| Ok((schema.schema_id, Arc::new(schema.try_into()?))))
                    .collect::<Result<Vec<_>, Error>>()?,
            );

            let current_schema: &SchemaRef =
                schemas.get(&value.current_schema_id).ok_or_else(|| {
                    Error::new(
                        ErrorKind::DataInvalid,
                        format!(
                            "No schema exists with the current schema id {}.",
                            value.current_schema_id
                        ),
                    )
                })?;
            let partition_specs = HashMap::from_iter(
                value
                    .partition_specs
                    .into_iter()
                    .map(|x| (x.spec_id(), Arc::new(x))),
            );
            let default_spec_id = value.default_spec_id;
            let default_spec: PartitionSpecRef = partition_specs
                .get(&value.default_spec_id)
                .map(|spec| (**spec).clone())
                .or_else(|| {
                    (DEFAULT_PARTITION_SPEC_ID == default_spec_id)
                        .then(PartitionSpec::unpartition_spec)
                })
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::DataInvalid,
                        format!("Default partition spec {default_spec_id} not found"),
                    )
                })?
                .into();
            let default_partition_type = default_spec.partition_type(current_schema)?;

            let mut metadata = TableMetadata {
                format_version: FormatVersion::V2,
                table_uuid: value.table_uuid,
                location: value.location,
                last_sequence_number: value.last_sequence_number,
                last_updated_ms: value.last_updated_ms,
                last_column_id: value.last_column_id,
                current_schema_id: value.current_schema_id,
                schemas,
                partition_specs,
                default_partition_type,
                default_spec,
                last_partition_id: value.last_partition_id,
                properties: value.properties.unwrap_or_default(),
                current_snapshot_id,
                snapshots: value
                    .snapshots
                    .map(|snapshots| {
                        HashMap::from_iter(
                            snapshots
                                .into_iter()
                                .map(|x| (x.snapshot_id, Arc::new(x.into()))),
                        )
                    })
                    .unwrap_or_default(),
                snapshot_log: value.snapshot_log.unwrap_or_default(),
                metadata_log: value.metadata_log.unwrap_or_default(),
                sort_orders: HashMap::from_iter(
                    value
                        .sort_orders
                        .into_iter()
                        .map(|x| (x.order_id, Arc::new(x))),
                ),
                default_sort_order_id: value.default_sort_order_id,
                refs: value.refs.unwrap_or_else(|| {
                    if let Some(snapshot_id) = current_snapshot_id {
                        HashMap::from_iter(vec![(MAIN_BRANCH.to_string(), SnapshotReference {
                            snapshot_id,
                            retention: SnapshotRetention::Branch {
                                min_snapshots_to_keep: None,
                                max_snapshot_age_ms: None,
                                max_ref_age_ms: None,
                            },
                        })])
                    } else {
                        HashMap::new()
                    }
                }),
                statistics: index_statistics(value.statistics),
                partition_statistics: index_partition_statistics(value.partition_statistics),
                encryption_keys: HashMap::new(),
            };

            metadata.borrow_mut().try_normalize()?;
            Ok(metadata)
        }
    }

    impl TryFrom<TableMetadataV1> for TableMetadata {
        type Error = Error;
        fn try_from(value: TableMetadataV1) -> Result<Self, Error> {
            let current_snapshot_id = if let &Some(-1) = &value.current_snapshot_id {
                None
            } else {
                value.current_snapshot_id
            };

            let (schemas, current_schema_id, current_schema) =
                if let (Some(schemas_vec), Some(schema_id)) =
                    (&value.schemas, value.current_schema_id)
                {
                    // Option 1: Use 'schemas' + 'current_schema_id'
                    let schema_map = HashMap::from_iter(
                        schemas_vec
                            .clone()
                            .into_iter()
                            .map(|schema| {
                                let schema: Schema = schema.try_into()?;
                                Ok((schema.schema_id(), Arc::new(schema)))
                            })
                            .collect::<Result<Vec<_>, Error>>()?,
                    );

                    let schema = schema_map
                        .get(&schema_id)
                        .ok_or_else(|| {
                            Error::new(
                                ErrorKind::DataInvalid,
                                format!(
                                    "No schema exists with the current schema id {}.",
                                    schema_id
                                ),
                            )
                        })?
                        .clone();
                    (schema_map, schema_id, schema)
                } else if let Some(schema) = value.schema {
                    // Option 2: Fall back to `schema`
                    let schema: Schema = schema.try_into()?;
                    let schema_id = schema.schema_id();
                    let schema_arc = Arc::new(schema);
                    let schema_map = HashMap::from_iter(vec![(schema_id, schema_arc.clone())]);
                    (schema_map, schema_id, schema_arc)
                } else {
                    // Option 3: No valid schema configuration found
                    return Err(Error::new(
                        ErrorKind::DataInvalid,
                        "No valid schema configuration found in table metadata",
                    ));
                };

            // Prioritize 'partition_specs' over 'partition_spec'
            let partition_specs = if let Some(specs_vec) = value.partition_specs {
                // Option 1: Use 'partition_specs'
                specs_vec
                    .into_iter()
                    .map(|x| (x.spec_id(), Arc::new(x)))
                    .collect::<HashMap<_, _>>()
            } else if let Some(partition_spec) = value.partition_spec {
                // Option 2: Fall back to 'partition_spec'
                let spec = PartitionSpec::builder(current_schema.clone())
                    .with_spec_id(DEFAULT_PARTITION_SPEC_ID)
                    .add_unbound_fields(partition_spec.into_iter().map(|f| f.into_unbound()))?
                    .build()?;

                HashMap::from_iter(vec![(DEFAULT_PARTITION_SPEC_ID, Arc::new(spec))])
            } else {
                // Option 3: Create empty partition spec
                let spec = PartitionSpec::builder(current_schema.clone())
                    .with_spec_id(DEFAULT_PARTITION_SPEC_ID)
                    .build()?;

                HashMap::from_iter(vec![(DEFAULT_PARTITION_SPEC_ID, Arc::new(spec))])
            };

            // Get the default_spec_id, prioritizing the explicit value if provided
            let default_spec_id = value
                .default_spec_id
                .unwrap_or_else(|| partition_specs.keys().copied().max().unwrap_or_default());

            // Get the default spec
            let default_spec: PartitionSpecRef = partition_specs
                .get(&default_spec_id)
                .map(|x| Arc::unwrap_or_clone(x.clone()))
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::DataInvalid,
                        format!("Default partition spec {default_spec_id} not found"),
                    )
                })?
                .into();
            let default_partition_type = default_spec.partition_type(&current_schema)?;

            let mut metadata = TableMetadata {
                format_version: FormatVersion::V1,
                table_uuid: value.table_uuid.unwrap_or_default(),
                location: value.location,
                last_sequence_number: 0,
                last_updated_ms: value.last_updated_ms,
                last_column_id: value.last_column_id,
                current_schema_id,
                default_spec,
                default_partition_type,
                last_partition_id: value
                    .last_partition_id
                    .unwrap_or_else(|| partition_specs.keys().copied().max().unwrap_or_default()),
                partition_specs,
                schemas,
                properties: value.properties.unwrap_or_default(),
                current_snapshot_id,
                snapshots: value
                    .snapshots
                    .map(|snapshots| {
                        Ok::<_, Error>(HashMap::from_iter(
                            snapshots
                                .into_iter()
                                .map(|x| Ok((x.snapshot_id, Arc::new(x.try_into()?))))
                                .collect::<Result<Vec<_>, Error>>()?,
                        ))
                    })
                    .transpose()?
                    .unwrap_or_default(),
                snapshot_log: value.snapshot_log.unwrap_or_default(),
                metadata_log: value.metadata_log.unwrap_or_default(),
                sort_orders: match value.sort_orders {
                    Some(sort_orders) => HashMap::from_iter(
                        sort_orders.into_iter().map(|x| (x.order_id, Arc::new(x))),
                    ),
                    None => HashMap::new(),
                },
                default_sort_order_id: value
                    .default_sort_order_id
                    .unwrap_or(SortOrder::UNSORTED_ORDER_ID),
                refs: if let Some(snapshot_id) = current_snapshot_id {
                    HashMap::from_iter(vec![(MAIN_BRANCH.to_string(), SnapshotReference {
                        snapshot_id,
                        retention: SnapshotRetention::Branch {
                            min_snapshots_to_keep: None,
                            max_snapshot_age_ms: None,
                            max_ref_age_ms: None,
                        },
                    })])
                } else {
                    HashMap::new()
                },
                statistics: index_statistics(value.statistics),
                partition_statistics: index_partition_statistics(value.partition_statistics),
                encryption_keys: HashMap::new(),
            };

            metadata.borrow_mut().try_normalize()?;
            Ok(metadata)
        }
    }

    impl From<TableMetadata> for TableMetadataV2 {
        fn from(v: TableMetadata) -> Self {
            TableMetadataV2 {
                format_version: VersionNumber::<2>,
                table_uuid: v.table_uuid,
                location: v.location,
                last_sequence_number: v.last_sequence_number,
                last_updated_ms: v.last_updated_ms,
                last_column_id: v.last_column_id,
                schemas: v
                    .schemas
                    .into_values()
                    .map(|x| {
                        Arc::try_unwrap(x)
                            .unwrap_or_else(|schema| schema.as_ref().clone())
                            .into()
                    })
                    .collect(),
                current_schema_id: v.current_schema_id,
                partition_specs: v
                    .partition_specs
                    .into_values()
                    .map(|x| Arc::try_unwrap(x).unwrap_or_else(|s| s.as_ref().clone()))
                    .collect(),
                default_spec_id: v.default_spec.spec_id(),
                last_partition_id: v.last_partition_id,
                properties: if v.properties.is_empty() {
                    None
                } else {
                    Some(v.properties)
                },
                current_snapshot_id: v.current_snapshot_id,
                snapshots: if v.snapshots.is_empty() {
                    None
                } else {
                    Some(
                        v.snapshots
                            .into_values()
                            .map(|x| {
                                Arc::try_unwrap(x)
                                    .unwrap_or_else(|snapshot| snapshot.as_ref().clone())
                                    .into()
                            })
                            .collect(),
                    )
                },
                snapshot_log: if v.snapshot_log.is_empty() {
                    None
                } else {
                    Some(v.snapshot_log)
                },
                metadata_log: if v.metadata_log.is_empty() {
                    None
                } else {
                    Some(v.metadata_log)
                },
                sort_orders: v
                    .sort_orders
                    .into_values()
                    .map(|x| Arc::try_unwrap(x).unwrap_or_else(|s| s.as_ref().clone()))
                    .collect(),
                default_sort_order_id: v.default_sort_order_id,
                refs: Some(v.refs),
                statistics: v.statistics.into_values().collect(),
                partition_statistics: v.partition_statistics.into_values().collect(),
            }
        }
    }

    impl TryFrom<TableMetadata> for TableMetadataV1 {
        type Error = Error;
        fn try_from(v: TableMetadata) -> Result<Self, Error> {
            Ok(TableMetadataV1 {
                format_version: VersionNumber::<1>,
                table_uuid: Some(v.table_uuid),
                location: v.location,
                last_updated_ms: v.last_updated_ms,
                last_column_id: v.last_column_id,
                schema: Some(
                    v.schemas
                        .get(&v.current_schema_id)
                        .ok_or(Error::new(
                            ErrorKind::Unexpected,
                            "current_schema_id not found in schemas",
                        ))?
                        .as_ref()
                        .clone()
                        .into(),
                ),
                schemas: Some(
                    v.schemas
                        .into_values()
                        .map(|x| {
                            Arc::try_unwrap(x)
                                .unwrap_or_else(|schema| schema.as_ref().clone())
                                .into()
                        })
                        .collect(),
                ),
                current_schema_id: Some(v.current_schema_id),
                partition_spec: Some(v.default_spec.fields().to_vec()),
                partition_specs: Some(
                    v.partition_specs
                        .into_values()
                        .map(|x| Arc::try_unwrap(x).unwrap_or_else(|s| s.as_ref().clone()))
                        .collect(),
                ),
                default_spec_id: Some(v.default_spec.spec_id()),
                last_partition_id: Some(v.last_partition_id),
                properties: if v.properties.is_empty() {
                    None
                } else {
                    Some(v.properties)
                },
                current_snapshot_id: v.current_snapshot_id,
                snapshots: if v.snapshots.is_empty() {
                    None
                } else {
                    Some(
                        v.snapshots
                            .into_values()
                            .map(|x| Snapshot::clone(&x).into())
                            .collect(),
                    )
                },
                snapshot_log: if v.snapshot_log.is_empty() {
                    None
                } else {
                    Some(v.snapshot_log)
                },
                metadata_log: if v.metadata_log.is_empty() {
                    None
                } else {
                    Some(v.metadata_log)
                },
                sort_orders: Some(
                    v.sort_orders
                        .into_values()
                        .map(|s| Arc::try_unwrap(s).unwrap_or_else(|s| s.as_ref().clone()))
                        .collect(),
                ),
                default_sort_order_id: Some(v.default_sort_order_id),
                statistics: v.statistics.into_values().collect(),
                partition_statistics: v.partition_statistics.into_values().collect(),
            })
        }
    }

    fn index_statistics(statistics: Vec<StatisticsFile>) -> HashMap<i64, StatisticsFile> {
        statistics
            .into_iter()
            .rev()
            .map(|s| (s.snapshot_id, s))
            .collect()
    }

    fn index_partition_statistics(
        statistics: Vec<PartitionStatisticsFile>,
    ) -> HashMap<i64, PartitionStatisticsFile> {
        statistics
            .into_iter()
            .rev()
            .map(|s| (s.snapshot_id, s))
            .collect()
    }
}

#[derive(Debug, Serialize_repr, Deserialize_repr, PartialEq, Eq, Clone, Copy, Hash)]
#[repr(u8)]
/// Iceberg format version
pub enum FormatVersion {
    /// Iceberg spec version 1
    V1 = 1u8,
    /// Iceberg spec version 2
    V2 = 2u8,
}

impl PartialOrd for FormatVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FormatVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        (*self as u8).cmp(&(*other as u8))
    }
}

impl Display for FormatVersion {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatVersion::V1 => write!(f, "v1"),
            FormatVersion::V2 => write!(f, "v2"),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
#[serde(rename_all = "kebab-case")]
/// Encodes changes to the previous metadata files for the table
pub struct MetadataLog {
    /// The file for the log.
    pub metadata_file: String,
    /// Time new metadata was created
    pub timestamp_ms: i64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
#[serde(rename_all = "kebab-case")]
/// A log of when each snapshot was made.
pub struct SnapshotLog {
    /// Id of the snapshot.
    pub snapshot_id: i64,
    /// Last updated timestamp
    pub timestamp_ms: i64,
}

impl SnapshotLog {
    /// Returns the last updated timestamp as a DateTime<Utc> with millisecond precision
    pub fn timestamp(self) -> Result<DateTime<Utc>> {
        timestamp_ms_to_utc(self.timestamp_ms)
    }

    /// Returns the timestamp in milliseconds
    #[inline]
    pub fn timestamp_ms(&self) -> i64 {
        self.timestamp_ms
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::sync::Arc;

    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::{FormatVersion, MetadataLog, SnapshotLog, TableMetadataBuilder};
    use crate::TableCreation;
    use crate::io::FileIOBuilder;
    use crate::spec::table_metadata::TableMetadata;
    use crate::spec::{
        BlobMetadata, NestedField, NullOrder, Operation, PartitionSpec, PartitionStatisticsFile,
        PrimitiveType, Schema, Snapshot, SnapshotReference, SnapshotRetention, SortDirection,
        SortField, SortOrder, StatisticsFile, Summary, Transform, Type, UnboundPartitionField,
    };

    fn check_table_metadata_serde(json: &str, expected_type: TableMetadata) {
        let desered_type: TableMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(desered_type, expected_type);

        let sered_json = serde_json::to_string(&expected_type).unwrap();
        let parsed_json_value = serde_json::from_str::<TableMetadata>(&sered_json).unwrap();

        assert_eq!(parsed_json_value, desered_type);
    }

    fn get_test_table_metadata(file_name: &str) -> TableMetadata {
        let path = format!("testdata/table_metadata/{}", file_name);
        let metadata: String = fs::read_to_string(path).unwrap();

        serde_json::from_str(&metadata).unwrap()
    }

    #[test]
    fn test_table_data_v2() {
        let data = r#"
            {
                "format-version" : 2,
                "table-uuid": "fb072c92-a02b-11e9-ae9c-1bb7bc9eca94",
                "location": "s3://b/wh/data.db/table",
                "last-sequence-number" : 1,
                "last-updated-ms": 1515100955770,
                "last-column-id": 1,
                "schemas": [
                    {
                        "schema-id" : 1,
                        "type" : "struct",
                        "fields" :[
                            {
                                "id": 1,
                                "name": "struct_name",
                                "required": true,
                                "type": "fixed[1]"
                            },
                            {
                                "id": 4,
                                "name": "ts",
                                "required": true,
                                "type": "timestamp"
                            }
                        ]
                    }
                ],
                "current-schema-id" : 1,
                "partition-specs": [
                    {
                        "spec-id": 0,
                        "fields": [
                            {
                                "source-id": 4,
                                "field-id": 1000,
                                "name": "ts_day",
                                "transform": "day"
                            }
                        ]
                    }
                ],
                "default-spec-id": 0,
                "last-partition-id": 1000,
                "properties": {
                    "commit.retry.num-retries": "1"
                },
                "metadata-log": [
                    {
                        "metadata-file": "s3://bucket/.../v1.json",
                        "timestamp-ms": 1515100
                    }
                ],
                "refs": {},
                "sort-orders": [
                    {
                    "order-id": 0,
                    "fields": []
                    }
                ],
                "default-sort-order-id": 0
            }
        "#;

        let schema = Schema::builder()
            .with_schema_id(1)
            .with_fields(vec![
                Arc::new(NestedField::required(
                    1,
                    "struct_name",
                    Type::Primitive(PrimitiveType::Fixed(1)),
                )),
                Arc::new(NestedField::required(
                    4,
                    "ts",
                    Type::Primitive(PrimitiveType::Timestamp),
                )),
            ])
            .build()
            .unwrap();

        let partition_spec = PartitionSpec::builder(schema.clone())
            .with_spec_id(0)
            .add_unbound_field(UnboundPartitionField {
                name: "ts_day".to_string(),
                transform: Transform::Day,
                source_id: 4,
                field_id: Some(1000),
            })
            .unwrap()
            .build()
            .unwrap();

        let default_partition_type = partition_spec.partition_type(&schema).unwrap();
        let expected = TableMetadata {
            format_version: FormatVersion::V2,
            table_uuid: Uuid::parse_str("fb072c92-a02b-11e9-ae9c-1bb7bc9eca94").unwrap(),
            location: "s3://b/wh/data.db/table".to_string(),
            last_updated_ms: 1515100955770,
            last_column_id: 1,
            schemas: HashMap::from_iter(vec![(1, Arc::new(schema))]),
            current_schema_id: 1,
            partition_specs: HashMap::from_iter(vec![(0, partition_spec.clone().into())]),
            default_partition_type,
            default_spec: partition_spec.into(),
            last_partition_id: 1000,
            default_sort_order_id: 0,
            sort_orders: HashMap::from_iter(vec![(0, SortOrder::unsorted_order().into())]),
            snapshots: HashMap::default(),
            current_snapshot_id: None,
            last_sequence_number: 1,
            properties: HashMap::from_iter(vec![(
                "commit.retry.num-retries".to_string(),
                "1".to_string(),
            )]),
            snapshot_log: Vec::new(),
            metadata_log: vec![MetadataLog {
                metadata_file: "s3://bucket/.../v1.json".to_string(),
                timestamp_ms: 1515100,
            }],
            refs: HashMap::new(),
            statistics: HashMap::new(),
            partition_statistics: HashMap::new(),
            encryption_keys: HashMap::new(),
        };

        let expected_json_value = serde_json::to_value(&expected).unwrap();
        check_table_metadata_serde(data, expected);

        let json_value = serde_json::from_str::<serde_json::Value>(data).unwrap();
        assert_eq!(json_value, expected_json_value);
    }

    #[test]
    fn test_table_data_v1() {
        let data = r#"
        {
            "format-version" : 1,
            "table-uuid" : "df838b92-0b32-465d-a44e-d39936e538b7",
            "location" : "/home/iceberg/warehouse/nyc/taxis",
            "last-updated-ms" : 1662532818843,
            "last-column-id" : 5,
            "schema" : {
              "type" : "struct",
              "schema-id" : 0,
              "fields" : [ {
                "id" : 1,
                "name" : "vendor_id",
                "required" : false,
                "type" : "long"
              }, {
                "id" : 2,
                "name" : "trip_id",
                "required" : false,
                "type" : "long"
              }, {
                "id" : 3,
                "name" : "trip_distance",
                "required" : false,
                "type" : "float"
              }, {
                "id" : 4,
                "name" : "fare_amount",
                "required" : false,
                "type" : "double"
              }, {
                "id" : 5,
                "name" : "store_and_fwd_flag",
                "required" : false,
                "type" : "string"
              } ]
            },
            "partition-spec" : [ {
              "name" : "vendor_id",
              "transform" : "identity",
              "source-id" : 1,
              "field-id" : 1000
            } ],
            "last-partition-id" : 1000,
            "default-sort-order-id" : 0,
            "sort-orders" : [ {
              "order-id" : 0,
              "fields" : [ ]
            } ],
            "properties" : {
              "owner" : "root"
            },
            "current-snapshot-id" : 638933773299822130,
            "refs" : {
              "main" : {
                "snapshot-id" : 638933773299822130,
                "type" : "branch"
              }
            },
            "snapshots" : [ {
              "snapshot-id" : 638933773299822130,
              "timestamp-ms" : 1662532818843,
              "sequence-number" : 0,
              "summary" : {
                "operation" : "append",
                "spark.app.id" : "local-1662532784305",
                "added-data-files" : "4",
                "added-records" : "4",
                "added-files-size" : "6001"
              },
              "manifest-list" : "/home/iceberg/warehouse/nyc/taxis/metadata/snap-638933773299822130-1-7e6760f0-4f6c-4b23-b907-0a5a174e3863.avro",
              "schema-id" : 0
            } ],
            "snapshot-log" : [ {
              "timestamp-ms" : 1662532818843,
              "snapshot-id" : 638933773299822130
            } ],
            "metadata-log" : [ {
              "timestamp-ms" : 1662532805245,
              "metadata-file" : "/home/iceberg/warehouse/nyc/taxis/metadata/00000-8a62c37d-4573-4021-952a-c0baef7d21d0.metadata.json"
            } ]
          }
        "#;

        let schema = Schema::builder()
            .with_fields(vec![
                Arc::new(NestedField::optional(
                    1,
                    "vendor_id",
                    Type::Primitive(PrimitiveType::Long),
                )),
                Arc::new(NestedField::optional(
                    2,
                    "trip_id",
                    Type::Primitive(PrimitiveType::Long),
                )),
                Arc::new(NestedField::optional(
                    3,
                    "trip_distance",
                    Type::Primitive(PrimitiveType::Float),
                )),
                Arc::new(NestedField::optional(
                    4,
                    "fare_amount",
                    Type::Primitive(PrimitiveType::Double),
                )),
                Arc::new(NestedField::optional(
                    5,
                    "store_and_fwd_flag",
                    Type::Primitive(PrimitiveType::String),
                )),
            ])
            .build()
            .unwrap();

        let schema = Arc::new(schema);
        let partition_spec = PartitionSpec::builder(schema.clone())
            .with_spec_id(0)
            .add_partition_field("vendor_id", "vendor_id", Transform::Identity)
            .unwrap()
            .build()
            .unwrap();

        let sort_order = SortOrder::builder()
            .with_order_id(0)
            .build_unbound()
            .unwrap();

        let snapshot = Snapshot::builder()
            .with_snapshot_id(638933773299822130)
            .with_timestamp_ms(1662532818843)
            .with_sequence_number(0)
            .with_schema_id(0)
            .with_manifest_list("/home/iceberg/warehouse/nyc/taxis/metadata/snap-638933773299822130-1-7e6760f0-4f6c-4b23-b907-0a5a174e3863.avro")
            .with_summary(Summary { operation: Operation::Append, additional_properties: HashMap::from_iter(vec![("spark.app.id".to_string(), "local-1662532784305".to_string()), ("added-data-files".to_string(), "4".to_string()), ("added-records".to_string(), "4".to_string()), ("added-files-size".to_string(), "6001".to_string())]) })
            .build();

        let default_partition_type = partition_spec.partition_type(&schema).unwrap();
        let expected = TableMetadata {
            format_version: FormatVersion::V1,
            table_uuid: Uuid::parse_str("df838b92-0b32-465d-a44e-d39936e538b7").unwrap(),
            location: "/home/iceberg/warehouse/nyc/taxis".to_string(),
            last_updated_ms: 1662532818843,
            last_column_id: 5,
            schemas: HashMap::from_iter(vec![(0, schema)]),
            current_schema_id: 0,
            partition_specs: HashMap::from_iter(vec![(0, partition_spec.clone().into())]),
            default_partition_type,
            default_spec: Arc::new(partition_spec),
            last_partition_id: 1000,
            default_sort_order_id: 0,
            sort_orders: HashMap::from_iter(vec![(0, sort_order.into())]),
            snapshots: HashMap::from_iter(vec![(638933773299822130, Arc::new(snapshot))]),
            current_snapshot_id: Some(638933773299822130),
            last_sequence_number: 0,
            properties: HashMap::from_iter(vec![("owner".to_string(), "root".to_string())]),
            snapshot_log: vec![SnapshotLog {
                snapshot_id: 638933773299822130,
                timestamp_ms: 1662532818843,
            }],
            metadata_log: vec![MetadataLog { metadata_file: "/home/iceberg/warehouse/nyc/taxis/metadata/00000-8a62c37d-4573-4021-952a-c0baef7d21d0.metadata.json".to_string(), timestamp_ms: 1662532805245 }],
            refs: HashMap::from_iter(vec![("main".to_string(), SnapshotReference { snapshot_id: 638933773299822130, retention: SnapshotRetention::Branch { min_snapshots_to_keep: None, max_snapshot_age_ms: None, max_ref_age_ms: None } })]),
            statistics: HashMap::new(),
            partition_statistics: HashMap::new(),
            encryption_keys: HashMap::new(),
        };

        check_table_metadata_serde(data, expected);
    }

    #[test]
    fn test_table_data_v2_no_snapshots() {
        let data = r#"
        {
            "format-version" : 2,
            "table-uuid": "fb072c92-a02b-11e9-ae9c-1bb7bc9eca94",
            "location": "s3://b/wh/data.db/table",
            "last-sequence-number" : 1,
            "last-updated-ms": 1515100955770,
            "last-column-id": 1,
            "schemas": [
                {
                    "schema-id" : 1,
                    "type" : "struct",
                    "fields" :[
                        {
                            "id": 1,
                            "name": "struct_name",
                            "required": true,
                            "type": "fixed[1]"
                        }
                    ]
                }
            ],
            "current-schema-id" : 1,
            "partition-specs": [
                {
                    "spec-id": 0,
                    "fields": []
                }
            ],
            "refs": {},
            "default-spec-id": 0,
            "last-partition-id": 1000,
            "metadata-log": [
                {
                    "metadata-file": "s3://bucket/.../v1.json",
                    "timestamp-ms": 1515100
                }
            ],
            "sort-orders": [
                {
                "order-id": 0,
                "fields": []
                }
            ],
            "default-sort-order-id": 0
        }
        "#;

        let schema = Schema::builder()
            .with_schema_id(1)
            .with_fields(vec![Arc::new(NestedField::required(
                1,
                "struct_name",
                Type::Primitive(PrimitiveType::Fixed(1)),
            ))])
            .build()
            .unwrap();

        let partition_spec = PartitionSpec::builder(schema.clone())
            .with_spec_id(0)
            .build()
            .unwrap();

        let default_partition_type = partition_spec.partition_type(&schema).unwrap();
        let expected = TableMetadata {
            format_version: FormatVersion::V2,
            table_uuid: Uuid::parse_str("fb072c92-a02b-11e9-ae9c-1bb7bc9eca94").unwrap(),
            location: "s3://b/wh/data.db/table".to_string(),
            last_updated_ms: 1515100955770,
            last_column_id: 1,
            schemas: HashMap::from_iter(vec![(1, Arc::new(schema))]),
            current_schema_id: 1,
            partition_specs: HashMap::from_iter(vec![(0, partition_spec.clone().into())]),
            default_partition_type,
            default_spec: partition_spec.into(),
            last_partition_id: 1000,
            default_sort_order_id: 0,
            sort_orders: HashMap::from_iter(vec![(0, SortOrder::unsorted_order().into())]),
            snapshots: HashMap::default(),
            current_snapshot_id: None,
            last_sequence_number: 1,
            properties: HashMap::new(),
            snapshot_log: Vec::new(),
            metadata_log: vec![MetadataLog {
                metadata_file: "s3://bucket/.../v1.json".to_string(),
                timestamp_ms: 1515100,
            }],
            refs: HashMap::new(),
            statistics: HashMap::new(),
            partition_statistics: HashMap::new(),
            encryption_keys: HashMap::new(),
        };

        let expected_json_value = serde_json::to_value(&expected).unwrap();
        check_table_metadata_serde(data, expected);

        let json_value = serde_json::from_str::<serde_json::Value>(data).unwrap();
        assert_eq!(json_value, expected_json_value);
    }

    #[test]
    fn test_current_snapshot_id_must_match_main_branch() {
        let data = r#"
        {
            "format-version" : 2,
            "table-uuid": "fb072c92-a02b-11e9-ae9c-1bb7bc9eca94",
            "location": "s3://b/wh/data.db/table",
            "last-sequence-number" : 1,
            "last-updated-ms": 1515100955770,
            "last-column-id": 1,
            "schemas": [
                {
                    "schema-id" : 1,
                    "type" : "struct",
                    "fields" :[
                        {
                            "id": 1,
                            "name": "struct_name",
                            "required": true,
                            "type": "fixed[1]"
                        },
                        {
                            "id": 4,
                            "name": "ts",
                            "required": true,
                            "type": "timestamp"
                        }
                    ]
                }
            ],
            "current-schema-id" : 1,
            "partition-specs": [
                {
                    "spec-id": 0,
                    "fields": [
                        {
                            "source-id": 4,
                            "field-id": 1000,
                            "name": "ts_day",
                            "transform": "day"
                        }
                    ]
                }
            ],
            "default-spec-id": 0,
            "last-partition-id": 1000,
            "properties": {
                "commit.retry.num-retries": "1"
            },
            "metadata-log": [
                {
                    "metadata-file": "s3://bucket/.../v1.json",
                    "timestamp-ms": 1515100
                }
            ],
            "sort-orders": [
                {
                "order-id": 0,
                "fields": []
                }
            ],
            "default-sort-order-id": 0,
            "current-snapshot-id" : 1,
            "refs" : {
              "main" : {
                "snapshot-id" : 2,
                "type" : "branch"
              }
            },
            "snapshots" : [ {
              "snapshot-id" : 1,
              "timestamp-ms" : 1662532818843,
              "sequence-number" : 0,
              "summary" : {
                "operation" : "append",
                "spark.app.id" : "local-1662532784305",
                "added-data-files" : "4",
                "added-records" : "4",
                "added-files-size" : "6001"
              },
              "manifest-list" : "/home/iceberg/warehouse/nyc/taxis/metadata/snap-638933773299822130-1-7e6760f0-4f6c-4b23-b907-0a5a174e3863.avro",
              "schema-id" : 0
            },
            {
              "snapshot-id" : 2,
              "timestamp-ms" : 1662532818844,
              "sequence-number" : 0,
              "summary" : {
                "operation" : "append",
                "spark.app.id" : "local-1662532784305",
                "added-data-files" : "4",
                "added-records" : "4",
                "added-files-size" : "6001"
              },
              "manifest-list" : "/home/iceberg/warehouse/nyc/taxis/metadata/snap-638933773299822130-1-7e6760f0-4f6c-4b23-b907-0a5a174e3863.avro",
              "schema-id" : 0
            } ]
        }
    "#;

        let err = serde_json::from_str::<TableMetadata>(data).unwrap_err();
        assert!(
            err.to_string()
                .contains("Current snapshot id does not match main branch")
        );
    }

    #[test]
    fn test_main_without_current() {
        let data = r#"
        {
            "format-version" : 2,
            "table-uuid": "fb072c92-a02b-11e9-ae9c-1bb7bc9eca94",
            "location": "s3://b/wh/data.db/table",
            "last-sequence-number" : 1,
            "last-updated-ms": 1515100955770,
            "last-column-id": 1,
            "schemas": [
                {
                    "schema-id" : 1,
                    "type" : "struct",
                    "fields" :[
                        {
                            "id": 1,
                            "name": "struct_name",
                            "required": true,
                            "type": "fixed[1]"
                        },
                        {
                            "id": 4,
                            "name": "ts",
                            "required": true,
                            "type": "timestamp"
                        }
                    ]
                }
            ],
            "current-schema-id" : 1,
            "partition-specs": [
                {
                    "spec-id": 0,
                    "fields": [
                        {
                            "source-id": 4,
                            "field-id": 1000,
                            "name": "ts_day",
                            "transform": "day"
                        }
                    ]
                }
            ],
            "default-spec-id": 0,
            "last-partition-id": 1000,
            "properties": {
                "commit.retry.num-retries": "1"
            },
            "metadata-log": [
                {
                    "metadata-file": "s3://bucket/.../v1.json",
                    "timestamp-ms": 1515100
                }
            ],
            "sort-orders": [
                {
                "order-id": 0,
                "fields": []
                }
            ],
            "default-sort-order-id": 0,
            "refs" : {
              "main" : {
                "snapshot-id" : 1,
                "type" : "branch"
              }
            },
            "snapshots" : [ {
              "snapshot-id" : 1,
              "timestamp-ms" : 1662532818843,
              "sequence-number" : 0,
              "summary" : {
                "operation" : "append",
                "spark.app.id" : "local-1662532784305",
                "added-data-files" : "4",
                "added-records" : "4",
                "added-files-size" : "6001"
              },
              "manifest-list" : "/home/iceberg/warehouse/nyc/taxis/metadata/snap-638933773299822130-1-7e6760f0-4f6c-4b23-b907-0a5a174e3863.avro",
              "schema-id" : 0
            } ]
        }
    "#;

        let err = serde_json::from_str::<TableMetadata>(data).unwrap_err();
        assert!(
            err.to_string()
                .contains("Current snapshot is not set, but main branch exists")
        );
    }

    #[test]
    fn test_branch_snapshot_missing() {
        let data = r#"
        {
            "format-version" : 2,
            "table-uuid": "fb072c92-a02b-11e9-ae9c-1bb7bc9eca94",
            "location": "s3://b/wh/data.db/table",
            "last-sequence-number" : 1,
            "last-updated-ms": 1515100955770,
            "last-column-id": 1,
            "schemas": [
                {
                    "schema-id" : 1,
                    "type" : "struct",
                    "fields" :[
                        {
                            "id": 1,
                            "name": "struct_name",
                            "required": true,
                            "type": "fixed[1]"
                        },
                        {
                            "id": 4,
                            "name": "ts",
                            "required": true,
                            "type": "timestamp"
                        }
                    ]
                }
            ],
            "current-schema-id" : 1,
            "partition-specs": [
                {
                    "spec-id": 0,
                    "fields": [
                        {
                            "source-id": 4,
                            "field-id": 1000,
                            "name": "ts_day",
                            "transform": "day"
                        }
                    ]
                }
            ],
            "default-spec-id": 0,
            "last-partition-id": 1000,
            "properties": {
                "commit.retry.num-retries": "1"
            },
            "metadata-log": [
                {
                    "metadata-file": "s3://bucket/.../v1.json",
                    "timestamp-ms": 1515100
                }
            ],
            "sort-orders": [
                {
                "order-id": 0,
                "fields": []
                }
            ],
            "default-sort-order-id": 0,
            "refs" : {
              "main" : {
                "snapshot-id" : 1,
                "type" : "branch"
              },
              "foo" : {
                "snapshot-id" : 2,
                "type" : "branch"
              }
            },
            "snapshots" : [ {
              "snapshot-id" : 1,
              "timestamp-ms" : 1662532818843,
              "sequence-number" : 0,
              "summary" : {
                "operation" : "append",
                "spark.app.id" : "local-1662532784305",
                "added-data-files" : "4",
                "added-records" : "4",
                "added-files-size" : "6001"
              },
              "manifest-list" : "/home/iceberg/warehouse/nyc/taxis/metadata/snap-638933773299822130-1-7e6760f0-4f6c-4b23-b907-0a5a174e3863.avro",
              "schema-id" : 0
            } ]
        }
    "#;

        let err = serde_json::from_str::<TableMetadata>(data).unwrap_err();
        assert!(
            err.to_string().contains(
                "Snapshot for reference foo does not exist in the existing snapshots list"
            )
        );
    }

    #[test]
    fn test_v2_wrong_max_snapshot_sequence_number() {
        let data = r#"
        {
            "format-version": 2,
            "table-uuid": "9c12d441-03fe-4693-9a96-a0705ddf69c1",
            "location": "s3://bucket/test/location",
            "last-sequence-number": 1,
            "last-updated-ms": 1602638573590,
            "last-column-id": 3,
            "current-schema-id": 0,
            "schemas": [
                {
                    "type": "struct",
                    "schema-id": 0,
                    "fields": [
                        {
                            "id": 1,
                            "name": "x",
                            "required": true,
                            "type": "long"
                        }
                    ]
                }
            ],
            "default-spec-id": 0,
            "partition-specs": [
                {
                    "spec-id": 0,
                    "fields": []
                }
            ],
            "last-partition-id": 1000,
            "default-sort-order-id": 0,
            "sort-orders": [
                {
                    "order-id": 0,
                    "fields": []
                }
            ],
            "properties": {},
            "current-snapshot-id": 3055729675574597004,
            "snapshots": [
                {
                    "snapshot-id": 3055729675574597004,
                    "timestamp-ms": 1555100955770,
                    "sequence-number": 4,
                    "summary": {
                        "operation": "append"
                    },
                    "manifest-list": "s3://a/b/2.avro",
                    "schema-id": 0
                }
            ],
            "statistics": [],
            "snapshot-log": [],
            "metadata-log": []
        }
    "#;

        let err = serde_json::from_str::<TableMetadata>(data).unwrap_err();
        println!("{}", err);
        assert!(err.to_string().contains(
            "Invalid snapshot with id 3055729675574597004 and sequence number 4 greater than last sequence number 1"
        ));

        // Change max sequence number to 4 - should work
        let data = data.replace(
            r#""last-sequence-number": 1,"#,
            r#""last-sequence-number": 4,"#,
        );
        let metadata = serde_json::from_str::<TableMetadata>(data.as_str()).unwrap();
        assert_eq!(metadata.last_sequence_number, 4);

        // Change max sequence number to 5 - should work
        let data = data.replace(
            r#""last-sequence-number": 4,"#,
            r#""last-sequence-number": 5,"#,
        );
        let metadata = serde_json::from_str::<TableMetadata>(data.as_str()).unwrap();
        assert_eq!(metadata.last_sequence_number, 5);
    }

    #[test]
    fn test_statistic_files() {
        let data = r#"
        {
            "format-version": 2,
            "table-uuid": "9c12d441-03fe-4693-9a96-a0705ddf69c1",
            "location": "s3://bucket/test/location",
            "last-sequence-number": 34,
            "last-updated-ms": 1602638573590,
            "last-column-id": 3,
            "current-schema-id": 0,
            "schemas": [
                {
                    "type": "struct",
                    "schema-id": 0,
                    "fields": [
                        {
                            "id": 1,
                            "name": "x",
                            "required": true,
                            "type": "long"
                        }
                    ]
                }
            ],
            "default-spec-id": 0,
            "partition-specs": [
                {
                    "spec-id": 0,
                    "fields": []
                }
            ],
            "last-partition-id": 1000,
            "default-sort-order-id": 0,
            "sort-orders": [
                {
                    "order-id": 0,
                    "fields": []
                }
            ],
            "properties": {},
            "current-snapshot-id": 3055729675574597004,
            "snapshots": [
                {
                    "snapshot-id": 3055729675574597004,
                    "timestamp-ms": 1555100955770,
                    "sequence-number": 1,
                    "summary": {
                        "operation": "append"
                    },
                    "manifest-list": "s3://a/b/2.avro",
                    "schema-id": 0
                }
            ],
            "statistics": [
                {
                    "snapshot-id": 3055729675574597004,
                    "statistics-path": "s3://a/b/stats.puffin",
                    "file-size-in-bytes": 413,
                    "file-footer-size-in-bytes": 42,
                    "blob-metadata": [
                        {
                            "type": "ndv",
                            "snapshot-id": 3055729675574597004,
                            "sequence-number": 1,
                            "fields": [
                                1
                            ]
                        }
                    ]
                }
            ],
            "snapshot-log": [],
            "metadata-log": []
        }
    "#;

        let schema = Schema::builder()
            .with_schema_id(0)
            .with_fields(vec![Arc::new(NestedField::required(
                1,
                "x",
                Type::Primitive(PrimitiveType::Long),
            ))])
            .build()
            .unwrap();
        let partition_spec = PartitionSpec::builder(schema.clone())
            .with_spec_id(0)
            .build()
            .unwrap();
        let snapshot = Snapshot::builder()
            .with_snapshot_id(3055729675574597004)
            .with_timestamp_ms(1555100955770)
            .with_sequence_number(1)
            .with_manifest_list("s3://a/b/2.avro")
            .with_schema_id(0)
            .with_summary(Summary {
                operation: Operation::Append,
                additional_properties: HashMap::new(),
            })
            .build();

        let default_partition_type = partition_spec.partition_type(&schema).unwrap();
        let expected = TableMetadata {
            format_version: FormatVersion::V2,
            table_uuid: Uuid::parse_str("9c12d441-03fe-4693-9a96-a0705ddf69c1").unwrap(),
            location: "s3://bucket/test/location".to_string(),
            last_updated_ms: 1602638573590,
            last_column_id: 3,
            schemas: HashMap::from_iter(vec![(0, Arc::new(schema))]),
            current_schema_id: 0,
            partition_specs: HashMap::from_iter(vec![(0, partition_spec.clone().into())]),
            default_partition_type,
            default_spec: Arc::new(partition_spec),
            last_partition_id: 1000,
            default_sort_order_id: 0,
            sort_orders: HashMap::from_iter(vec![(0, SortOrder::unsorted_order().into())]),
            snapshots: HashMap::from_iter(vec![(3055729675574597004, Arc::new(snapshot))]),
            current_snapshot_id: Some(3055729675574597004),
            last_sequence_number: 34,
            properties: HashMap::new(),
            snapshot_log: Vec::new(),
            metadata_log: Vec::new(),
            statistics: HashMap::from_iter(vec![(3055729675574597004, StatisticsFile {
                snapshot_id: 3055729675574597004,
                statistics_path: "s3://a/b/stats.puffin".to_string(),
                file_size_in_bytes: 413,
                file_footer_size_in_bytes: 42,
                key_metadata: None,
                blob_metadata: vec![BlobMetadata {
                    snapshot_id: 3055729675574597004,
                    sequence_number: 1,
                    fields: vec![1],
                    r#type: "ndv".to_string(),
                    properties: HashMap::new(),
                }],
            })]),
            partition_statistics: HashMap::new(),
            refs: HashMap::from_iter(vec![("main".to_string(), SnapshotReference {
                snapshot_id: 3055729675574597004,
                retention: SnapshotRetention::Branch {
                    min_snapshots_to_keep: None,
                    max_snapshot_age_ms: None,
                    max_ref_age_ms: None,
                },
            })]),
            encryption_keys: HashMap::new(),
        };

        check_table_metadata_serde(data, expected);
    }

    #[test]
    fn test_partition_statistics_file() {
        let data = r#"
        {
            "format-version": 2,
            "table-uuid": "9c12d441-03fe-4693-9a96-a0705ddf69c1",
            "location": "s3://bucket/test/location",
            "last-sequence-number": 34,
            "last-updated-ms": 1602638573590,
            "last-column-id": 3,
            "current-schema-id": 0,
            "schemas": [
                {
                    "type": "struct",
                    "schema-id": 0,
                    "fields": [
                        {
                            "id": 1,
                            "name": "x",
                            "required": true,
                            "type": "long"
                        }
                    ]
                }
            ],
            "default-spec-id": 0,
            "partition-specs": [
                {
                    "spec-id": 0,
                    "fields": []
                }
            ],
            "last-partition-id": 1000,
            "default-sort-order-id": 0,
            "sort-orders": [
                {
                    "order-id": 0,
                    "fields": []
                }
            ],
            "properties": {},
            "current-snapshot-id": 3055729675574597004,
            "snapshots": [
                {
                    "snapshot-id": 3055729675574597004,
                    "timestamp-ms": 1555100955770,
                    "sequence-number": 1,
                    "summary": {
                        "operation": "append"
                    },
                    "manifest-list": "s3://a/b/2.avro",
                    "schema-id": 0
                }
            ],
            "partition-statistics": [
                {
                    "snapshot-id": 3055729675574597004,
                    "statistics-path": "s3://a/b/partition-stats.parquet",
                    "file-size-in-bytes": 43
                }
            ],
            "snapshot-log": [],
            "metadata-log": []
        }
        "#;

        let schema = Schema::builder()
            .with_schema_id(0)
            .with_fields(vec![Arc::new(NestedField::required(
                1,
                "x",
                Type::Primitive(PrimitiveType::Long),
            ))])
            .build()
            .unwrap();
        let partition_spec = PartitionSpec::builder(schema.clone())
            .with_spec_id(0)
            .build()
            .unwrap();
        let snapshot = Snapshot::builder()
            .with_snapshot_id(3055729675574597004)
            .with_timestamp_ms(1555100955770)
            .with_sequence_number(1)
            .with_manifest_list("s3://a/b/2.avro")
            .with_schema_id(0)
            .with_summary(Summary {
                operation: Operation::Append,
                additional_properties: HashMap::new(),
            })
            .build();

        let default_partition_type = partition_spec.partition_type(&schema).unwrap();
        let expected = TableMetadata {
            format_version: FormatVersion::V2,
            table_uuid: Uuid::parse_str("9c12d441-03fe-4693-9a96-a0705ddf69c1").unwrap(),
            location: "s3://bucket/test/location".to_string(),
            last_updated_ms: 1602638573590,
            last_column_id: 3,
            schemas: HashMap::from_iter(vec![(0, Arc::new(schema))]),
            current_schema_id: 0,
            partition_specs: HashMap::from_iter(vec![(0, partition_spec.clone().into())]),
            default_spec: Arc::new(partition_spec),
            default_partition_type,
            last_partition_id: 1000,
            default_sort_order_id: 0,
            sort_orders: HashMap::from_iter(vec![(0, SortOrder::unsorted_order().into())]),
            snapshots: HashMap::from_iter(vec![(3055729675574597004, Arc::new(snapshot))]),
            current_snapshot_id: Some(3055729675574597004),
            last_sequence_number: 34,
            properties: HashMap::new(),
            snapshot_log: Vec::new(),
            metadata_log: Vec::new(),
            statistics: HashMap::new(),
            partition_statistics: HashMap::from_iter(vec![(
                3055729675574597004,
                PartitionStatisticsFile {
                    snapshot_id: 3055729675574597004,
                    statistics_path: "s3://a/b/partition-stats.parquet".to_string(),
                    file_size_in_bytes: 43,
                },
            )]),
            refs: HashMap::from_iter(vec![("main".to_string(), SnapshotReference {
                snapshot_id: 3055729675574597004,
                retention: SnapshotRetention::Branch {
                    min_snapshots_to_keep: None,
                    max_snapshot_age_ms: None,
                    max_ref_age_ms: None,
                },
            })]),
            encryption_keys: HashMap::new(),
        };

        check_table_metadata_serde(data, expected);
    }

    #[test]
    fn test_invalid_table_uuid() -> Result<()> {
        let data = r#"
            {
                "format-version" : 2,
                "table-uuid": "xxxx"
            }
        "#;
        assert!(serde_json::from_str::<TableMetadata>(data).is_err());
        Ok(())
    }

    #[test]
    fn test_deserialize_table_data_v2_invalid_format_version() -> Result<()> {
        let data = r#"
            {
                "format-version" : 1
            }
        "#;
        assert!(serde_json::from_str::<TableMetadata>(data).is_err());
        Ok(())
    }

    #[test]
    fn test_table_metadata_v2_file_valid() {
        let metadata =
            fs::read_to_string("testdata/table_metadata/TableMetadataV2Valid.json").unwrap();

        let schema1 = Schema::builder()
            .with_schema_id(0)
            .with_fields(vec![Arc::new(NestedField::required(
                1,
                "x",
                Type::Primitive(PrimitiveType::Long),
            ))])
            .build()
            .unwrap();

        let schema2 = Schema::builder()
            .with_schema_id(1)
            .with_fields(vec![
                Arc::new(NestedField::required(
                    1,
                    "x",
                    Type::Primitive(PrimitiveType::Long),
                )),
                Arc::new(
                    NestedField::required(2, "y", Type::Primitive(PrimitiveType::Long))
                        .with_doc("comment"),
                ),
                Arc::new(NestedField::required(
                    3,
                    "z",
                    Type::Primitive(PrimitiveType::Long),
                )),
            ])
            .with_identifier_field_ids(vec![1, 2])
            .build()
            .unwrap();

        let partition_spec = PartitionSpec::builder(schema2.clone())
            .with_spec_id(0)
            .add_unbound_field(UnboundPartitionField {
                name: "x".to_string(),
                transform: Transform::Identity,
                source_id: 1,
                field_id: Some(1000),
            })
            .unwrap()
            .build()
            .unwrap();

        let sort_order = SortOrder::builder()
            .with_order_id(3)
            .with_sort_field(SortField {
                source_id: 2,
                transform: Transform::Identity,
                direction: SortDirection::Ascending,
                null_order: NullOrder::First,
            })
            .with_sort_field(SortField {
                source_id: 3,
                transform: Transform::Bucket(4),
                direction: SortDirection::Descending,
                null_order: NullOrder::Last,
            })
            .build_unbound()
            .unwrap();

        let snapshot1 = Snapshot::builder()
            .with_snapshot_id(3051729675574597004)
            .with_timestamp_ms(1515100955770)
            .with_sequence_number(0)
            .with_manifest_list("s3://a/b/1.avro")
            .with_summary(Summary {
                operation: Operation::Append,
                additional_properties: HashMap::new(),
            })
            .build();

        let snapshot2 = Snapshot::builder()
            .with_snapshot_id(3055729675574597004)
            .with_parent_snapshot_id(Some(3051729675574597004))
            .with_timestamp_ms(1555100955770)
            .with_sequence_number(1)
            .with_schema_id(1)
            .with_manifest_list("s3://a/b/2.avro")
            .with_summary(Summary {
                operation: Operation::Append,
                additional_properties: HashMap::new(),
            })
            .build();

        let default_partition_type = partition_spec.partition_type(&schema2).unwrap();
        let expected = TableMetadata {
            format_version: FormatVersion::V2,
            table_uuid: Uuid::parse_str("9c12d441-03fe-4693-9a96-a0705ddf69c1").unwrap(),
            location: "s3://bucket/test/location".to_string(),
            last_updated_ms: 1602638573590,
            last_column_id: 3,
            schemas: HashMap::from_iter(vec![(0, Arc::new(schema1)), (1, Arc::new(schema2))]),
            current_schema_id: 1,
            partition_specs: HashMap::from_iter(vec![(0, partition_spec.clone().into())]),
            default_spec: Arc::new(partition_spec),
            default_partition_type,
            last_partition_id: 1000,
            default_sort_order_id: 3,
            sort_orders: HashMap::from_iter(vec![(3, sort_order.into())]),
            snapshots: HashMap::from_iter(vec![
                (3051729675574597004, Arc::new(snapshot1)),
                (3055729675574597004, Arc::new(snapshot2)),
            ]),
            current_snapshot_id: Some(3055729675574597004),
            last_sequence_number: 34,
            properties: HashMap::new(),
            snapshot_log: vec![
                SnapshotLog {
                    snapshot_id: 3051729675574597004,
                    timestamp_ms: 1515100955770,
                },
                SnapshotLog {
                    snapshot_id: 3055729675574597004,
                    timestamp_ms: 1555100955770,
                },
            ],
            metadata_log: Vec::new(),
            refs: HashMap::from_iter(vec![("main".to_string(), SnapshotReference {
                snapshot_id: 3055729675574597004,
                retention: SnapshotRetention::Branch {
                    min_snapshots_to_keep: None,
                    max_snapshot_age_ms: None,
                    max_ref_age_ms: None,
                },
            })]),
            statistics: HashMap::new(),
            partition_statistics: HashMap::new(),
            encryption_keys: HashMap::new(),
        };

        check_table_metadata_serde(&metadata, expected);
    }

    #[test]
    fn test_table_metadata_v2_file_valid_minimal() {
        let metadata =
            fs::read_to_string("testdata/table_metadata/TableMetadataV2ValidMinimal.json").unwrap();

        let schema = Schema::builder()
            .with_schema_id(0)
            .with_fields(vec![
                Arc::new(NestedField::required(
                    1,
                    "x",
                    Type::Primitive(PrimitiveType::Long),
                )),
                Arc::new(
                    NestedField::required(2, "y", Type::Primitive(PrimitiveType::Long))
                        .with_doc("comment"),
                ),
                Arc::new(NestedField::required(
                    3,
                    "z",
                    Type::Primitive(PrimitiveType::Long),
                )),
            ])
            .build()
            .unwrap();

        let partition_spec = PartitionSpec::builder(schema.clone())
            .with_spec_id(0)
            .add_unbound_field(UnboundPartitionField {
                name: "x".to_string(),
                transform: Transform::Identity,
                source_id: 1,
                field_id: Some(1000),
            })
            .unwrap()
            .build()
            .unwrap();

        let sort_order = SortOrder::builder()
            .with_order_id(3)
            .with_sort_field(SortField {
                source_id: 2,
                transform: Transform::Identity,
                direction: SortDirection::Ascending,
                null_order: NullOrder::First,
            })
            .with_sort_field(SortField {
                source_id: 3,
                transform: Transform::Bucket(4),
                direction: SortDirection::Descending,
                null_order: NullOrder::Last,
            })
            .build_unbound()
            .unwrap();

        let default_partition_type = partition_spec.partition_type(&schema).unwrap();
        let expected = TableMetadata {
            format_version: FormatVersion::V2,
            table_uuid: Uuid::parse_str("9c12d441-03fe-4693-9a96-a0705ddf69c1").unwrap(),
            location: "s3://bucket/test/location".to_string(),
            last_updated_ms: 1602638573590,
            last_column_id: 3,
            schemas: HashMap::from_iter(vec![(0, Arc::new(schema))]),
            current_schema_id: 0,
            partition_specs: HashMap::from_iter(vec![(0, partition_spec.clone().into())]),
            default_partition_type,
            default_spec: Arc::new(partition_spec),
            last_partition_id: 1000,
            default_sort_order_id: 3,
            sort_orders: HashMap::from_iter(vec![(3, sort_order.into())]),
            snapshots: HashMap::default(),
            current_snapshot_id: None,
            last_sequence_number: 34,
            properties: HashMap::new(),
            snapshot_log: vec![],
            metadata_log: Vec::new(),
            refs: HashMap::new(),
            statistics: HashMap::new(),
            partition_statistics: HashMap::new(),
            encryption_keys: HashMap::new(),
        };

        check_table_metadata_serde(&metadata, expected);
    }

    #[test]
    fn test_table_metadata_v1_file_valid() {
        let metadata =
            fs::read_to_string("testdata/table_metadata/TableMetadataV1Valid.json").unwrap();

        let schema = Schema::builder()
            .with_schema_id(0)
            .with_fields(vec![
                Arc::new(NestedField::required(
                    1,
                    "x",
                    Type::Primitive(PrimitiveType::Long),
                )),
                Arc::new(
                    NestedField::required(2, "y", Type::Primitive(PrimitiveType::Long))
                        .with_doc("comment"),
                ),
                Arc::new(NestedField::required(
                    3,
                    "z",
                    Type::Primitive(PrimitiveType::Long),
                )),
            ])
            .build()
            .unwrap();

        let partition_spec = PartitionSpec::builder(schema.clone())
            .with_spec_id(0)
            .add_unbound_field(UnboundPartitionField {
                name: "x".to_string(),
                transform: Transform::Identity,
                source_id: 1,
                field_id: Some(1000),
            })
            .unwrap()
            .build()
            .unwrap();

        let default_partition_type = partition_spec.partition_type(&schema).unwrap();
        let expected = TableMetadata {
            format_version: FormatVersion::V1,
            table_uuid: Uuid::parse_str("d20125c8-7284-442c-9aea-15fee620737c").unwrap(),
            location: "s3://bucket/test/location".to_string(),
            last_updated_ms: 1602638573874,
            last_column_id: 3,
            schemas: HashMap::from_iter(vec![(0, Arc::new(schema))]),
            current_schema_id: 0,
            partition_specs: HashMap::from_iter(vec![(0, partition_spec.clone().into())]),
            default_spec: Arc::new(partition_spec),
            default_partition_type,
            last_partition_id: 0,
            default_sort_order_id: 0,
            // Sort order is added during deserialization for V2 compatibility
            sort_orders: HashMap::from_iter(vec![(0, SortOrder::unsorted_order().into())]),
            snapshots: HashMap::new(),
            current_snapshot_id: None,
            last_sequence_number: 0,
            properties: HashMap::new(),
            snapshot_log: vec![],
            metadata_log: Vec::new(),
            refs: HashMap::new(),
            statistics: HashMap::new(),
            partition_statistics: HashMap::new(),
            encryption_keys: HashMap::new(),
        };

        check_table_metadata_serde(&metadata, expected);
    }

    #[test]
    fn test_table_metadata_v1_compat() {
        let metadata =
            fs::read_to_string("testdata/table_metadata/TableMetadataV1Compat.json").unwrap();

        // Deserialize the JSON to verify it works
        let desered_type: TableMetadata = serde_json::from_str(&metadata)
            .expect("Failed to deserialize TableMetadataV1Compat.json");

        // Verify some key fields match
        assert_eq!(desered_type.format_version(), FormatVersion::V1);
        assert_eq!(
            desered_type.uuid(),
            Uuid::parse_str("3276010d-7b1d-488c-98d8-9025fc4fde6b").unwrap()
        );
        assert_eq!(
            desered_type.location(),
            "s3://bucket/warehouse/iceberg/glue.db/table_name"
        );
        assert_eq!(desered_type.last_updated_ms(), 1727773114005);
        assert_eq!(desered_type.current_schema_id(), 0);
    }

    #[test]
    fn test_table_metadata_v1_schemas_without_current_id() {
        let metadata = fs::read_to_string(
            "testdata/table_metadata/TableMetadataV1SchemasWithoutCurrentId.json",
        )
        .unwrap();

        // Deserialize the JSON - this should succeed by using the 'schema' field instead of 'schemas'
        let desered_type: TableMetadata = serde_json::from_str(&metadata)
            .expect("Failed to deserialize TableMetadataV1SchemasWithoutCurrentId.json");

        // Verify it used the 'schema' field
        assert_eq!(desered_type.format_version(), FormatVersion::V1);
        assert_eq!(
            desered_type.uuid(),
            Uuid::parse_str("d20125c8-7284-442c-9aea-15fee620737c").unwrap()
        );

        // Get the schema and verify it has the expected fields
        let schema = desered_type.current_schema();
        assert_eq!(schema.as_struct().fields().len(), 3);
        assert_eq!(schema.as_struct().fields()[0].name, "x");
        assert_eq!(schema.as_struct().fields()[1].name, "y");
        assert_eq!(schema.as_struct().fields()[2].name, "z");
    }

    #[test]
    fn test_table_metadata_v1_no_valid_schema() {
        let metadata =
            fs::read_to_string("testdata/table_metadata/TableMetadataV1NoValidSchema.json")
                .unwrap();

        // Deserialize the JSON - this should fail because neither schemas + current_schema_id nor schema is valid
        let desered: Result<TableMetadata, serde_json::Error> = serde_json::from_str(&metadata);

        assert!(desered.is_err());
        let error_message = desered.unwrap_err().to_string();
        assert!(
            error_message.contains("No valid schema configuration found"),
            "Expected error about no valid schema configuration, got: {}",
            error_message
        );
    }

    #[test]
    fn test_table_metadata_v1_partition_specs_without_default_id() {
        let metadata = fs::read_to_string(
            "testdata/table_metadata/TableMetadataV1PartitionSpecsWithoutDefaultId.json",
        )
        .unwrap();

        // Deserialize the JSON - this should succeed by inferring default_spec_id as the max spec ID
        let desered_type: TableMetadata = serde_json::from_str(&metadata)
            .expect("Failed to deserialize TableMetadataV1PartitionSpecsWithoutDefaultId.json");

        // Verify basic metadata
        assert_eq!(desered_type.format_version(), FormatVersion::V1);
        assert_eq!(
            desered_type.uuid(),
            Uuid::parse_str("d20125c8-7284-442c-9aea-15fee620737c").unwrap()
        );

        // Verify partition specs
        assert_eq!(desered_type.default_partition_spec_id(), 2); // Should pick the largest spec ID (2)
        assert_eq!(desered_type.partition_specs.len(), 2);

        // Verify the default spec has the expected fields
        let default_spec = &desered_type.default_spec;
        assert_eq!(default_spec.spec_id(), 2);
        assert_eq!(default_spec.fields().len(), 1);
        assert_eq!(default_spec.fields()[0].name, "y");
        assert_eq!(default_spec.fields()[0].transform, Transform::Identity);
        assert_eq!(default_spec.fields()[0].source_id, 2);
    }

    #[test]
    fn test_table_metadata_v2_schema_not_found() {
        let metadata =
            fs::read_to_string("testdata/table_metadata/TableMetadataV2CurrentSchemaNotFound.json")
                .unwrap();

        let desered: Result<TableMetadata, serde_json::Error> = serde_json::from_str(&metadata);

        assert_eq!(
            desered.unwrap_err().to_string(),
            "DataInvalid => No schema exists with the current schema id 2."
        )
    }

    #[test]
    fn test_table_metadata_v2_missing_sort_order() {
        let metadata =
            fs::read_to_string("testdata/table_metadata/TableMetadataV2MissingSortOrder.json")
                .unwrap();

        let desered: Result<TableMetadata, serde_json::Error> = serde_json::from_str(&metadata);

        assert_eq!(
            desered.unwrap_err().to_string(),
            "data did not match any variant of untagged enum TableMetadataEnum"
        )
    }

    #[test]
    fn test_table_metadata_v2_missing_partition_specs() {
        let metadata =
            fs::read_to_string("testdata/table_metadata/TableMetadataV2MissingPartitionSpecs.json")
                .unwrap();

        let desered: Result<TableMetadata, serde_json::Error> = serde_json::from_str(&metadata);

        assert_eq!(
            desered.unwrap_err().to_string(),
            "data did not match any variant of untagged enum TableMetadataEnum"
        )
    }

    #[test]
    fn test_table_metadata_v2_missing_last_partition_id() {
        let metadata = fs::read_to_string(
            "testdata/table_metadata/TableMetadataV2MissingLastPartitionId.json",
        )
        .unwrap();

        let desered: Result<TableMetadata, serde_json::Error> = serde_json::from_str(&metadata);

        assert_eq!(
            desered.unwrap_err().to_string(),
            "data did not match any variant of untagged enum TableMetadataEnum"
        )
    }

    #[test]
    fn test_table_metadata_v2_missing_schemas() {
        let metadata =
            fs::read_to_string("testdata/table_metadata/TableMetadataV2MissingSchemas.json")
                .unwrap();

        let desered: Result<TableMetadata, serde_json::Error> = serde_json::from_str(&metadata);

        assert_eq!(
            desered.unwrap_err().to_string(),
            "data did not match any variant of untagged enum TableMetadataEnum"
        )
    }

    #[test]
    fn test_table_metadata_v2_unsupported_version() {
        let metadata =
            fs::read_to_string("testdata/table_metadata/TableMetadataUnsupportedVersion.json")
                .unwrap();

        let desered: Result<TableMetadata, serde_json::Error> = serde_json::from_str(&metadata);

        assert_eq!(
            desered.unwrap_err().to_string(),
            "data did not match any variant of untagged enum TableMetadataEnum"
        )
    }

    #[test]
    fn test_order_of_format_version() {
        assert!(FormatVersion::V1 < FormatVersion::V2);
        assert_eq!(FormatVersion::V1, FormatVersion::V1);
        assert_eq!(FormatVersion::V2, FormatVersion::V2);
    }

    #[test]
    fn test_default_partition_spec() {
        let default_spec_id = 1234;
        let mut table_meta_data = get_test_table_metadata("TableMetadataV2Valid.json");
        let partition_spec = PartitionSpec::unpartition_spec();
        table_meta_data.default_spec = partition_spec.clone().into();
        table_meta_data
            .partition_specs
            .insert(default_spec_id, Arc::new(partition_spec));

        assert_eq!(
            (*table_meta_data.default_partition_spec().clone()).clone(),
            (*table_meta_data
                .partition_spec_by_id(default_spec_id)
                .unwrap()
                .clone())
            .clone()
        );
    }
    #[test]
    fn test_default_sort_order() {
        let default_sort_order_id = 1234;
        let mut table_meta_data = get_test_table_metadata("TableMetadataV2Valid.json");
        table_meta_data.default_sort_order_id = default_sort_order_id;
        table_meta_data
            .sort_orders
            .insert(default_sort_order_id, Arc::new(SortOrder::default()));

        assert_eq!(
            table_meta_data.default_sort_order(),
            table_meta_data
                .sort_orders
                .get(&default_sort_order_id)
                .unwrap()
        )
    }

    #[test]
    fn test_table_metadata_builder_from_table_creation() {
        let table_creation = TableCreation::builder()
            .location("s3://db/table".to_string())
            .name("table".to_string())
            .properties(HashMap::new())
            .schema(Schema::builder().build().unwrap())
            .build();
        let table_metadata = TableMetadataBuilder::from_table_creation(table_creation)
            .unwrap()
            .build()
            .unwrap()
            .metadata;
        assert_eq!(table_metadata.location, "s3://db/table");
        assert_eq!(table_metadata.schemas.len(), 1);
        assert_eq!(
            table_metadata
                .schemas
                .get(&0)
                .unwrap()
                .as_struct()
                .fields()
                .len(),
            0
        );
        assert_eq!(table_metadata.properties.len(), 0);
        assert_eq!(
            table_metadata.partition_specs,
            HashMap::from([(
                0,
                Arc::new(
                    PartitionSpec::builder(table_metadata.schemas.get(&0).unwrap().clone())
                        .with_spec_id(0)
                        .build()
                        .unwrap()
                )
            )])
        );
        assert_eq!(
            table_metadata.sort_orders,
            HashMap::from([(
                0,
                Arc::new(SortOrder {
                    order_id: 0,
                    fields: vec![]
                })
            )])
        );
    }

    #[tokio::test]
    async fn test_table_metadata_read_write() {
        // Create a temporary directory for our test
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path().to_str().unwrap();

        // Create a FileIO instance
        let file_io = FileIOBuilder::new_fs_io().build().unwrap();

        // Use an existing test metadata from the test files
        let original_metadata: TableMetadata = get_test_table_metadata("TableMetadataV2Valid.json");

        // Define the metadata location
        let metadata_location = format!("{}/metadata.json", temp_path);

        // Write the metadata
        original_metadata
            .write_to(&file_io, &metadata_location)
            .await
            .unwrap();

        // Verify the file exists
        assert!(fs::metadata(&metadata_location).is_ok());

        // Read the metadata back
        let read_metadata = TableMetadata::read_from(&file_io, &metadata_location)
            .await
            .unwrap();

        // Verify the metadata matches
        assert_eq!(read_metadata, original_metadata);
    }

    #[tokio::test]
    async fn test_table_metadata_read_nonexistent_file() {
        // Create a FileIO instance
        let file_io = FileIOBuilder::new_fs_io().build().unwrap();

        // Try to read a non-existent file
        let result = TableMetadata::read_from(&file_io, "/nonexistent/path/metadata.json").await;

        // Verify it returns an error
        assert!(result.is_err());
    }
}
