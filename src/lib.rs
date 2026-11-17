//! Fastener archive engine.
//!
//! Content-aware boundaries, independent compression chunks, and checksums are
//! processed on the CPU for portable and predictable operation.

mod analyzer;
mod archive;
mod bundle;
mod encrypted;
mod file_ops;
mod path_safety;
mod recovery;

pub use recovery::{
    RecoveryInfo, RecoveryLimits, RecoveryReport, create_recovery_with_progress, recovery_info,
    recovery_path, recovery_plan, repair_with_all_limits_and_progress,
    repair_with_archive_limits_and_progress, repair_with_progress,
    repair_with_zip_limits_and_progress, repaired_path,
};

pub use encrypted::{
    ENCRYPTED_MAGIC, EncryptedLimits, EncryptedReport, compress_encrypted_with_progress,
    decompress_encrypted_with_limits_and_progress, decompress_encrypted_with_progress,
    encrypted_is_directory, verify_encrypted_with_limits_and_progress,
    verify_encrypted_with_progress,
};

pub use analyzer::{AnalysisBackend, AnalysisReport, analyze};
pub use archive::{
    ArchiveStats, CompressOptions, FstLimits, VerifyReport, compress_bytes, decompress_bytes,
    decompress_bytes_with_limits, inspect_archive, inspect_archive_with_limits, verify_bytes,
    verify_bytes_with_limits,
};
pub use bundle::{
    DIRECTORY_MAGIC, DirectoryReport, compress_directory_bundle_with_progress,
    decompress_directory_bundle_with_limits_and_progress,
    decompress_directory_bundle_with_progress, verify_directory_bundle_with_limits_and_progress,
    verify_directory_bundle_with_progress,
};
pub use file_ops::{
    ProgressInfo, ProgressPhase, ZipCompressionReport, ZipLimits, ZipReport, compress_file,
    compress_file_with_progress, compress_zip_file, compress_zip_file_with_progress,
    decompress_file, decompress_file_with_limits_and_progress, decompress_file_with_progress,
    extract_zip_file, extract_zip_file_with_limits, extract_zip_file_with_limits_and_progress,
    extract_zip_file_with_progress, verify_file, verify_file_with_limits_and_progress,
    verify_file_with_progress, verify_zip_file, verify_zip_file_with_limits,
    verify_zip_file_with_limits_and_progress, verify_zip_file_with_progress,
};
