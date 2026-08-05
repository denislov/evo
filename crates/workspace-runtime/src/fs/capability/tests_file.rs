use super::*;

#[cfg(test)]
mod binding_capacity_tests {
    use super::*;

    #[tokio::test]
    async fn binding_table_rejects_capacity_overflow_and_reports_oldest_age() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("file.txt"), "content").expect("write fixture");
        let capability =
            FilesystemCapability::new(temp.path().to_path_buf()).expect("filesystem capability");

        for index in 0..MAX_FILESYSTEM_BINDINGS {
            capability
                .bind_tool_target(
                    "capacity-operation",
                    &format!("call-{index}"),
                    "read",
                    "file.txt",
                )
                .await
                .expect("binding within capacity");
        }
        assert_eq!(capability.bound_len(), MAX_FILESYSTEM_BINDINGS);

        let error = capability
            .bind_tool_target("capacity-operation", "overflow", "read", "file.txt")
            .await
            .expect_err("binding beyond capacity must fail closed");
        let message = error.to_string();
        assert!(message.contains("binding table capacity exceeded"));
        assert!(message.contains("oldest binding age"));
        assert_eq!(capability.bound_len(), MAX_FILESYSTEM_BINDINGS);

        capability.discard_operation_bindings("capacity-operation");
        assert_eq!(capability.bound_len(), 0);
    }
}

#[cfg(test)]
mod symlink_escape_tests {
    use super::*;

    fn capability(root: &std::path::Path) -> FilesystemCapability {
        FilesystemCapability::new(root.to_path_buf()).expect("capability opens")
    }

    #[test]
    fn read_through_a_workspace_symlink_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside = temp.path().join("outside-secret");
        std::fs::write(&outside, "secret").expect("write outside file");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("create workspace");
        std::fs::create_dir(workspace.join("sub")).expect("create subdir");
        // A symlink inside the workspace pointing at the outside directory.
        std::os::unix::fs::symlink(&outside, workspace.join("sub").join("link")).expect("symlink");

        let capability = capability(&workspace);
        let error = capability
            .prepare_target_blocking("read", "sub/link")
            .expect_err("a workspace symlink must be rejected");
        let WorkspaceError::UnsupportedCapability {
            capability: message,
        } = &error
        else {
            panic!("expected UnsupportedCapability, got {error:?}");
        };
        assert!(
            message.contains("symbolic link"),
            "rejection must mention the symlink, got: {message}"
        );
    }

    #[test]
    fn write_through_a_workspace_symlink_parent_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside = temp.path().join("outside-dir");
        std::fs::create_dir(&outside).expect("create outside dir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("create workspace");
        std::os::unix::fs::symlink(&outside, workspace.join("linked")).expect("symlink");

        let capability = capability(&workspace);
        let error = capability
            .prepare_target_blocking("write", "linked/new-file.txt")
            .expect_err("writing through a workspace symlink parent must be rejected");
        assert!(
            error.to_string().contains("symbolic link"),
            "rejection must mention the symlink, got: {error}"
        );
    }

    #[test]
    fn plain_workspace_paths_still_open() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("create workspace");
        std::fs::create_dir(workspace.join("sub")).expect("create subdir");
        std::fs::write(workspace.join("sub").join("file.txt"), "hello").expect("write file");

        let capability = capability(&workspace);
        let target = capability
            .prepare_target_blocking("read", "sub/file.txt")
            .expect("a plain workspace path opens");
        assert!(target.object.is_some());
    }
}
