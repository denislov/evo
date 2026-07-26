use std::path::PathBuf;

use futures::future::BoxFuture;

use crate::execution::FileError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub name: String,
    pub path: PathBuf,
    pub kind: FileKind,
    pub len: Option<u64>,
}

pub trait FileSystem: Send + Sync {
    fn cwd(&self) -> PathBuf;
    fn absolute_path<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<PathBuf, FileError>>;
    fn join_path<'a>(&'a self, parts: &'a [&'a str]) -> BoxFuture<'a, Result<PathBuf, FileError>>;
    fn read_text_file<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String, FileError>>;
    fn read_text_lines<'a>(
        &'a self,
        path: &'a str,
        max_lines: Option<usize>,
    ) -> BoxFuture<'a, Result<Vec<String>, FileError>>;
    fn read_binary_file<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<u8>, FileError>>;
    fn write_file<'a>(
        &'a self,
        path: &'a str,
        content: &'a [u8],
    ) -> BoxFuture<'a, Result<(), FileError>>;
    fn append_file<'a>(
        &'a self,
        path: &'a str,
        content: &'a [u8],
    ) -> BoxFuture<'a, Result<(), FileError>>;
    fn file_info<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<FileInfo, FileError>>;
    fn list_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<FileInfo>, FileError>>;
    fn canonical_path<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<PathBuf, FileError>>;
    fn exists<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<bool, FileError>>;
    fn create_dir<'a>(
        &'a self,
        path: &'a str,
        recursive: bool,
    ) -> BoxFuture<'a, Result<(), FileError>>;
    fn remove<'a>(
        &'a self,
        path: &'a str,
        recursive: bool,
        force: bool,
    ) -> BoxFuture<'a, Result<(), FileError>>;
    fn create_temp_dir<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<PathBuf, FileError>>;
    fn create_temp_file<'a>(
        &'a self,
        prefix: &'a str,
        suffix: &'a str,
    ) -> BoxFuture<'a, Result<PathBuf, FileError>>;
    fn cleanup_files<'a>(&'a self) -> BoxFuture<'a, ()>;
}
