use super::*;

#[test]
fn the_variable_budget_is_an_eighth_of_data_within_its_bounds() {
    // 4 GiB of 4 KiB blocks, 262144 inodes: 512 MiB and 32768 inodes, both capped.
    assert_eq!(variable_budget(4096, 1 << 20, 262_144), (262_144, 16384));
    // 1 GiB, 65536 inodes: 128 MiB and 8192 inodes, inside the bounds.
    assert_eq!(variable_budget(4096, 1 << 18, 65536), (131_072, 8192));
    // 128 MiB, 8192 inodes: 16 MiB and 1024 inodes, raised to the floor.
    assert_eq!(variable_budget(4096, 1 << 15, 8192), (32768, 2048));
}

#[test]
fn a_symlink_in_place_of_a_project_directory_is_refused() {
    let data = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), data.path().join("mica")).unwrap();
    let error = data_projects(data.path()).unwrap_err();
    assert!(
        error.to_string().contains("is not a directory"),
        "{error:#}"
    );
}
