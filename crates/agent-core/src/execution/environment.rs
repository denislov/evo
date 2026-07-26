use futures::future::{BoxFuture, FutureExt};

use super::{FileSystem, Shell};

pub trait ExecutionEnv: FileSystem + Shell {
    fn cleanup<'a>(&'a self) -> BoxFuture<'a, ()> {
        async move {
            self.cleanup_shell().await;
            self.cleanup_files().await;
        }
        .boxed()
    }
}

impl<T> ExecutionEnv for T where T: FileSystem + Shell {}
