use std::{fs, path::Path};

use fontbrew_core::{
    activation::{
        deactivate, ActivationArtifact, ActivationPlan, ActivationPlanner, ActivationRequest,
        ActivationStrategy,
    },
    ExecutionPolicy, FontbrewError, PackageId, PlanRisk,
};

fn package_id(id: &str) -> PackageId {
    PackageId::parse(id).expect("test package id should be valid")
}

fn write_font(path: &Path) {
    fs::write(path, b"font bytes").expect("write font");
}

#[tokio::test]
async fn symlink_activation_creates_tracked_artifacts_in_activation_dir() {
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
        strategy: ActivationStrategy::Symlink,
    };
    let plan = ActivationPlanner::plan(request).expect("plan activation");

    assert!(plan.risks.is_empty());
    assert_eq!(plan.artifacts.len(), 1);
    assert_eq!(
        plan.artifacts[0].path,
        activation_dir.join("Inter-Regular.ttf")
    );

    let artifacts = plan
        .apply(ExecutionPolicy::SafeOnly)
        .expect("apply safe activation");

    assert_eq!(artifacts, plan.artifacts);
    assert_eq!(
        fs::read_link(activation_dir.join("Inter-Regular.ttf")).expect("activation symlink"),
        font_path
    );
}

#[tokio::test]
async fn deactivation_removes_only_tracked_artifacts_in_activation_dir() {
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
        strategy: ActivationStrategy::Symlink,
    }];

    deactivate(&activation_dir, &artifacts).expect("deactivate artifacts");

    assert!(!artifact_path.exists());
    assert!(activation_dir.exists());
}

#[tokio::test]
async fn deactivation_rejects_and_preserves_plain_file_at_tracked_symlink_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let activation_dir = temp.path().join("activation");
    let source_path = temp.path().join("source.ttf");
    let artifact_path = activation_dir.join("Inter-Regular.ttf");
    fs::create_dir_all(&activation_dir).expect("create activation dir");
    fs::write(&source_path, b"source").expect("write source");
    fs::write(&artifact_path, b"unmanaged").expect("write unmanaged file");

    let artifacts = vec![ActivationArtifact {
        package_id: package_id("inter"),
        path: artifact_path.clone(),
        source_path,
        strategy: ActivationStrategy::Symlink,
    }];

    let error = deactivate(&activation_dir, &artifacts).expect_err("plain file should reject");

    assert!(matches!(error, FontbrewError::Conflict { .. }));
    assert_eq!(
        fs::read(&artifact_path).expect("unmanaged file should remain"),
        b"unmanaged"
    );
}

#[tokio::test]
async fn deactivation_rejects_and_preserves_symlink_to_different_source() {
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
        strategy: ActivationStrategy::Symlink,
    }];

    let error = deactivate(&activation_dir, &artifacts)
        .expect_err("symlink to different source should reject");

    assert!(matches!(error, FontbrewError::Conflict { .. }));
    assert_eq!(
        fs::read_link(&artifact_path).expect("unmanaged symlink should remain"),
        other_source_path
    );
}

#[tokio::test]
async fn symlink_activation_reports_unmanaged_conflict_without_overwriting() {
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
        strategy: ActivationStrategy::Symlink,
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

#[tokio::test]
async fn deactivation_rejects_artifacts_outside_activation_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let activation_dir = temp.path().join("activation");
    let outside_path = temp.path().join("outside.ttf");
    fs::create_dir_all(&activation_dir).expect("create activation dir");
    fs::write(&outside_path, b"outside").expect("write outside artifact");

    let artifacts = vec![ActivationArtifact {
        package_id: package_id("inter"),
        path: outside_path.clone(),
        source_path: temp.path().join("source.ttf"),
        strategy: ActivationStrategy::Symlink,
    }];

    let error = deactivate(&activation_dir, &artifacts).expect_err("outside path should reject");

    assert!(matches!(error, FontbrewError::PathResolution { .. }));
    assert!(outside_path.exists());
}

#[tokio::test]
async fn activation_apply_rejects_artifacts_outside_activation_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let activation_dir = temp.path().join("activation");
    let source_path = temp.path().join("source.ttf");
    let outside_path = temp.path().join("outside.ttf");
    fs::write(&source_path, b"source").expect("write source");

    let plan = ActivationPlan {
        package_id: package_id("inter"),
        activation_dir,
        strategy: ActivationStrategy::Symlink,
        artifacts: vec![ActivationArtifact {
            package_id: package_id("inter"),
            path: outside_path.clone(),
            source_path,
            strategy: ActivationStrategy::Symlink,
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
#[tokio::test]
async fn activation_apply_rejects_symlink_directory_component_under_activation_dir() {
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
        strategy: ActivationStrategy::Symlink,
        artifacts: vec![ActivationArtifact {
            package_id: package_id("inter"),
            path: artifact_path,
            source_path,
            strategy: ActivationStrategy::Symlink,
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
#[tokio::test]
async fn deactivation_rejects_symlink_directory_component_under_activation_dir() {
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
        strategy: ActivationStrategy::Symlink,
    }];

    let error =
        deactivate(&activation_dir, &artifacts).expect_err("symlink ancestor should reject");

    assert!(matches!(error, FontbrewError::PathResolution { .. }));
    assert_eq!(
        fs::read(&outside_artifact_path).expect("outside target should remain"),
        b"outside target"
    );
}
