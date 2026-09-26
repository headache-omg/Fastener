//! Fastener archive engine.
//!
//! Content-aware boundaries, independent compression chunks, and checksums are
//! processed on the CPU for portable and predictable operation.

mod analyzer;
mod archive;
mod bundle;
mod encrypted;
mod file_ops;
mod recovery;

pub use recovery::{
    RecoveryInfo, RecoveryReport, create_recovery_with_progress, recovery_info, recovery_path,
    recovery_plan, repair_with_progress, repaired_path,
};

pub use encrypted::{
    ENCRYPTED_MAGIC, EncryptedReport, compress_encrypted_with_progress,
    decompress_encrypted_with_progress, encrypted_is_directory, verify_encrypted_with_progress,
};

pub use analyzer::{AnalysisBackend, AnalysisReport, analyze};
pub use archive::{
    ArchiveStats, CompressOptions, VerifyReport, compress_bytes, decompress_bytes, inspect_archive,
    verify_bytes,
};
pub use bundle::{
    DIRECTORY_MAGIC, DirectoryReport, compress_directory_bundle_with_progress,
    decompress_directory_bundle_with_progress, verify_directory_bundle_with_progress,
};
pub use file_ops::{
    ProgressInfo, ProgressPhase, ZipCompressionReport, ZipReport, compress_file,
    compress_file_with_progress, compress_zip_file, compress_zip_file_with_progress,
    decompress_file, decompress_file_with_progress, extract_zip_file,
    extract_zip_file_with_progress, verify_file, verify_file_with_progress, verify_zip_file,
    verify_zip_file_with_progress,
};
