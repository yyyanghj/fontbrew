use std::{fs, path::Path};

use fontbrew_core::{
    activation::{
        deactivate, ActivationArtifact, ActivationPlan, ActivationPlanner, ActivationRequest,
    },
    manifest::ManifestActivationStrategy,
    ExecutionPolicy, FontbrewError, PackageId, PlanRisk,
};

fn package_id(id: &str) -> PackageId {
    PackageId::parse(id).expect("test package id should be valid")
}

fn write_font(path: &Path) {
    fs::write(path, b"font bytes").expect("write font");
}

#[test]
fn activation_creates_tracked_copies_in_activation_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("package-fonts");
    let activation_dir = temp.path().join("activation");
    fs::create_dir_all(&source_dir).expect("create source dir");
    let font_path = source_dir.join("Inter-Regular.ttf");
    write_font(&font_path);

    let request = ActivationRequest {
        package_id: package_id("inter"),
        font_files: vec![font_path.clone()],
        activation_dir: activation_dir.clone(),
    };
    let plan = ActivationPlanner::plan(request).expect("plan activation");
    let artifacts = plan
        .apply(ExecutionPolicy::SafeOnly)
        .expect("apply safe activation");

    let activation_path = activation_dir.join("Inter-Regular.ttf");
    assert_eq!(artifacts, plan.artifacts);
    assert_eq!(artifacts[0].strategy, ManifestActivationStrategy::Copy);
    assert_eq!(
        fs::read(&activation_path).expect("activation copy"),
        fs::read(&font_path).expect("source font")
    );
    assert!(!fs::symlink_metadata(&activation_path)
        .expect("activation metadata")
        .file_type()
        .is_symlink());
}

#[test]
fn deactivation_removes_legacy_tracked_symlink() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("package-fonts");
    let activation_dir = temp.path().join("activation");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&activation_dir).expect("create activation dir");
    let font_path = source_dir.join("Inter-Regular.ttf");
    write_font(&font_path);
    let artifact_path = activation_dir.join("Inter-Regular.ttf");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&font_path, &artifact_path).expect("create activation symlink");

    let artifacts = vec![ActivationArtifact {
        package_id: package_id("inter"),
        path: artifact_path.clone(),
        source_path: font_path,
        strategy: ManifestActivationStrategy::Symlink,
    }];

    deactivate(&activation_dir, &artifacts).expect("deactivate artifacts");

    assert!(!artifact_path.exists());
    assert!(activation_dir.exists());
}

#[test]
fn copy_deactivation_removes_matching_tracked_copy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let activation_dir = temp.path().join("activation");
    let source_path = temp.path().join("source.ttf");
    let artifact_path = activation_dir.join("Inter-Regular.ttf");
    fs::create_dir_all(&activation_dir).expect("create activation dir");
    fs::write(&source_path, b"source").expect("write source");
    fs::write(&artifact_path, b"source").expect("write activation copy");

    let artifacts = vec![ActivationArtifact {
        package_id: package_id("inter"),
        path: artifact_path.clone(),
        source_path,
        strategy: ManifestActivationStrategy::Copy,
    }];

    deactivate(&activation_dir, &artifacts).expect("deactivate copy artifacts");

    assert!(!artifact_path.exists());
    assert!(activation_dir.exists());
}

#[test]
fn copy_deactivation_prevalidates_all_artifacts_before_removal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let activation_dir = temp.path().join("activation");
    let first_source_path = temp.path().join("first-source.ttf");
    let second_source_path = temp.path().join("second-source.ttf");
    let first_artifact_path = activation_dir.join("Inter-Regular.ttf");
    let second_artifact_path = activation_dir.join("Inter-Bold.ttf");
    fs::create_dir_all(&activation_dir).expect("create activation dir");
    fs::write(&first_source_path, b"first source").expect("write first source");
    fs::write(&second_source_path, b"second source").expect("write second source");
    fs::write(&first_artifact_path, b"first source").expect("write first activation copy");
    fs::write(&second_artifact_path, b"changed").expect("write changed activation copy");

    let artifacts = vec![
        ActivationArtifact {
            package_id: package_id("inter"),
            path: first_artifact_path.clone(),
            source_path: first_source_path,
            strategy: ManifestActivationStrategy::Copy,
        },
        ActivationArtifact {
            package_id: package_id("inter"),
            path: second_artifact_path.clone(),
            source_path: second_source_path,
            strategy: ManifestActivationStrategy::Copy,
        },
    ];

    let error = deactivate(&activation_dir, &artifacts).expect_err("changed copy should reject");

    assert!(matches!(error, FontbrewError::Conflict { .. }));
    assert_eq!(
        fs::read(&first_artifact_path).expect("first copy should remain"),
        b"first source"
    );
    assert_eq!(
        fs::read(&second_artifact_path).expect("changed copy should remain"),
        b"changed"
    );
}

#[cfg(unix)]
#[test]
fn copy_deactivation_rejects_symlink_even_when_it_points_to_managed_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let activation_dir = temp.path().join("activation");
    let source_path = temp.path().join("source.ttf");
    let artifact_path = activation_dir.join("Inter-Regular.ttf");
    fs::create_dir_all(&activation_dir).expect("create activation dir");
    fs::write(&source_path, b"source").expect("write source");
    std::os::unix::fs::symlink(&source_path, &artifact_path).expect("replace copy with symlink");

    let artifacts = vec![ActivationArtifact {
        package_id: package_id("inter"),
        path: artifact_path.clone(),
        source_path: source_path.clone(),
        strategy: ManifestActivationStrategy::Copy,
    }];

    let error = deactivate(&activation_dir, &artifacts)
        .expect_err("copy record must not clean a replacement symlink");

    assert!(matches!(error, FontbrewError::Conflict { .. }));
    assert_eq!(
        fs::read_link(&artifact_path).expect("replacement symlink should remain"),
        source_path
    );
}

#[test]
fn deactivation_rejects_and_preserves_symlink_to_different_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let activation_dir = temp.path().join("activation");
    let source_path = temp.path().join("source.ttf");
    let other_source_path = temp.path().join("other-source.ttf");
    let artifact_path = activation_dir.join("Inter-Regular.ttf");
    fs::create_dir_all(&activation_dir).expect("create activation dir");
    fs::write(&source_path, b"source").expect("write source");
    fs::write(&other_source_path, b"other source").expect("write other source");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&other_source_path, &artifact_path)
        .expect("create unmanaged symlink");

    let artifacts = vec![ActivationArtifact {
        package_id: package_id("inter"),
        path: artifact_path.clone(),
        source_path,
        strategy: ManifestActivationStrategy::Symlink,
    }];

    let error = deactivate(&activation_dir, &artifacts)
        .expect_err("symlink to different source should reject");

    assert!(matches!(error, FontbrewError::Conflict { .. }));
    assert_eq!(
        fs::read_link(&artifact_path).expect("unmanaged symlink should remain"),
        other_source_path
    );
}

#[test]
fn activation_reports_unmanaged_conflict_without_overwriting() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("package-fonts");
    let activation_dir = temp.path().join("activation");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&activation_dir).expect("create activation dir");
    let font_path = source_dir.join("Inter-Regular.ttf");
    write_font(&font_path);
    let conflict_path = activation_dir.join("Inter-Regular.ttf");
    fs::write(&conflict_path, b"unmanaged").expect("write conflict");

    let request = ActivationRequest {
        package_id: package_id("inter"),
        font_files: vec![font_path],
        activation_dir,
    };
    let plan = ActivationPlanner::plan(request).expect("plan activation");

    assert_eq!(plan.risks.len(), 1);
    let error = plan
        .apply(ExecutionPolicy::SafeOnly)
        .expect_err("safe apply should reject risky plan");

    assert!(matches!(
        error,
        FontbrewError::ExecutionPolicyRequired { .. }
    ));
    assert_eq!(
        fs::read(&conflict_path).expect("conflict file should remain"),
        b"unmanaged"
    );
}

#[test]
fn deactivation_rejects_artifacts_outside_activation_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let activation_dir = temp.path().join("activation");
    let outside_path = temp.path().join("outside.ttf");
    fs::create_dir_all(&activation_dir).expect("create activation dir");
    fs::write(&outside_path, b"outside").expect("write outside artifact");

    let artifacts = vec![ActivationArtifact {
        package_id: package_id("inter"),
        path: outside_path.clone(),
        source_path: temp.path().join("source.ttf"),
        strategy: ManifestActivationStrategy::Copy,
    }];

    let error = deactivate(&activation_dir, &artifacts).expect_err("outside path should reject");

    assert!(matches!(error, FontbrewError::PathResolution { .. }));
    assert!(outside_path.exists());
}

#[test]
fn activation_apply_rejects_artifacts_outside_activation_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let activation_dir = temp.path().join("activation");
    let source_path = temp.path().join("source.ttf");
    let outside_path = temp.path().join("outside.ttf");
    fs::write(&source_path, b"source").expect("write source");

    let plan = ActivationPlan {
        package_id: package_id("inter"),
        activation_dir,
        artifacts: vec![ActivationArtifact {
            package_id: package_id("inter"),
            path: outside_path.clone(),
            source_path,
            strategy: ManifestActivationStrategy::Copy,
        }],
        risks: Vec::<PlanRisk>::new(),
    };

    let error = plan
        .apply(ExecutionPolicy::SafeOnly)
        .expect_err("outside artifact should reject");

    assert!(matches!(error, FontbrewError::PathResolution { .. }));
    assert!(!outside_path.exists());
}

#[cfg(unix)]
#[test]
fn activation_failure_preserves_preexisting_symlink_and_cleans_only_created_copies() {
    let temp = tempfile::tempdir().expect("tempdir");
    let activation_dir = temp.path().join("activation");
    let first_source = temp.path().join("first-source.ttf");
    let second_source = temp.path().join("second-source.ttf");
    let first_artifact = activation_dir.join("First.ttf");
    let second_artifact = activation_dir.join("Second.ttf");
    fs::create_dir_all(&activation_dir).expect("create activation dir");
    fs::write(&first_source, b"first").expect("write first source");
    fs::write(&second_source, b"second").expect("write second source");
    std::os::unix::fs::symlink(&second_source, &second_artifact)
        .expect("create preexisting symlink");

    let plan = ActivationPlan {
        package_id: package_id("inter"),
        activation_dir,
        artifacts: vec![
            ActivationArtifact {
                package_id: package_id("inter"),
                path: first_artifact.clone(),
                source_path: first_source,
                strategy: ManifestActivationStrategy::Copy,
            },
            ActivationArtifact {
                package_id: package_id("inter"),
                path: second_artifact.clone(),
                source_path: second_source.clone(),
                strategy: ManifestActivationStrategy::Copy,
            },
        ],
        risks: Vec::new(),
    };

    let error = plan
        .apply(ExecutionPolicy::SafeOnly)
        .expect_err("preexisting symlink should block activation");

    assert!(matches!(error, FontbrewError::Conflict { .. }));
    assert!(!first_artifact.exists());
    assert_eq!(
        fs::read_link(&second_artifact).expect("preexisting symlink should remain"),
        second_source
    );
}

#[cfg(unix)]
#[test]
fn activation_apply_rejects_symlink_directory_component_under_activation_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let activation_dir = temp.path().join("activation");
    let outside_dir = temp.path().join("outside");
    let source_path = temp.path().join("source.ttf");
    let symlink_component = activation_dir.join("linked");
    let artifact_path = symlink_component.join("Inter-Regular.ttf");
    let outside_artifact_path = outside_dir.join("Inter-Regular.ttf");
    fs::create_dir_all(&activation_dir).expect("create activation dir");
    fs::create_dir_all(&outside_dir).expect("create outside dir");
    fs::write(&source_path, b"source").expect("write source");
    std::os::unix::fs::symlink(&outside_dir, &symlink_component).expect("create symlink component");

    let plan = ActivationPlan {
        package_id: package_id("inter"),
        activation_dir,
        artifacts: vec![ActivationArtifact {
            package_id: package_id("inter"),
            path: artifact_path,
            source_path,
            strategy: ManifestActivationStrategy::Copy,
        }],
        risks: Vec::<PlanRisk>::new(),
    };

    let error = plan
        .apply(ExecutionPolicy::SafeOnly)
        .expect_err("symlink ancestor should reject");

    assert!(matches!(error, FontbrewError::PathResolution { .. }));
    assert!(!outside_artifact_path.exists());
}

#[cfg(unix)]
#[test]
fn deactivation_rejects_symlink_directory_component_under_activation_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let activation_dir = temp.path().join("activation");
    let outside_dir = temp.path().join("outside");
    let source_path = temp.path().join("source.ttf");
    let symlink_component = activation_dir.join("linked");
    let artifact_path = symlink_component.join("Inter-Regular.ttf");
    let outside_artifact_path = outside_dir.join("Inter-Regular.ttf");
    fs::create_dir_all(&activation_dir).expect("create activation dir");
    fs::create_dir_all(&outside_dir).expect("create outside dir");
    fs::write(&source_path, b"source").expect("write source");
    fs::write(&outside_artifact_path, b"outside target").expect("write outside target");
    std::os::unix::fs::symlink(&outside_dir, &symlink_component).expect("create symlink component");

    let artifacts = vec![ActivationArtifact {
        package_id: package_id("inter"),
        path: artifact_path,
        source_path,
        strategy: ManifestActivationStrategy::Copy,
    }];

    let error =
        deactivate(&activation_dir, &artifacts).expect_err("symlink ancestor should reject");

    assert!(matches!(error, FontbrewError::PathResolution { .. }));
    assert_eq!(
        fs::read(&outside_artifact_path).expect("outside target should remain"),
        b"outside target"
    );
}
