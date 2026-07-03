use crate::error::{FontbrewError, Result};
use crate::install;
use crate::model::{
    CancellationToken, ExecutionPolicy, InfoReport, InfoRequest, InstallPlan, InstallReport,
    InstallRequest, ListReport, OutdatedReport, OutdatedRequest, ProgressSink, RemovePlan,
    RemoveReport, RemoveRequest, SearchReport, SearchRequest, UpdatePlan, UpdateReport,
    UpdateRequest,
};
use crate::platform::FontbrewPaths;

#[derive(Debug, Default, Clone)]
pub struct FontbrewApp {
    paths: Option<FontbrewPaths>,
}

impl FontbrewApp {
    pub fn new() -> Self {
        Self { paths: None }
    }

    pub fn with_paths(paths: FontbrewPaths) -> Self {
        Self { paths: Some(paths) }
    }

    pub fn install_plan(&self, request: InstallRequest) -> Result<InstallPlan> {
        if !matches!(request.source, crate::InstallSource::LocalPath(_)) {
            return not_implemented("install_plan");
        }

        install::install_plan(&self.paths()?, request)
    }

    pub fn apply_install(
        &self,
        plan: InstallPlan,
        policy: ExecutionPolicy,
        progress: &mut dyn ProgressSink,
        cancellation: &dyn CancellationToken,
    ) -> Result<InstallReport> {
        install::apply_install(&self.paths()?, plan, policy, progress, cancellation)
    }

    pub fn list_packages(&self) -> Result<ListReport> {
        install::list_packages(&self.paths()?)
    }

    pub fn package_info(&self, request: InfoRequest) -> Result<InfoReport> {
        install::package_info(&self.paths()?, request)
    }

    pub fn remove_plan(&self, request: RemoveRequest) -> Result<RemovePlan> {
        install::remove_plan(&self.paths()?, request)
    }

    pub fn apply_remove(
        &self,
        plan: RemovePlan,
        policy: ExecutionPolicy,
        progress: &mut dyn ProgressSink,
        cancellation: &dyn CancellationToken,
    ) -> Result<RemoveReport> {
        install::apply_remove(&self.paths()?, plan, policy, progress, cancellation)
    }

    pub fn outdated(&self, _request: OutdatedRequest) -> Result<OutdatedReport> {
        not_implemented("outdated")
    }

    pub fn update_plan(
        &self,
        _request: UpdateRequest,
        _progress: &mut dyn ProgressSink,
        _cancellation: &dyn CancellationToken,
    ) -> Result<UpdatePlan> {
        not_implemented("update_plan")
    }

    pub fn apply_update(
        &self,
        _plan: UpdatePlan,
        _policy: ExecutionPolicy,
        _progress: &mut dyn ProgressSink,
        _cancellation: &dyn CancellationToken,
    ) -> Result<UpdateReport> {
        not_implemented("apply_update")
    }

    pub fn search(&self, _request: SearchRequest) -> Result<SearchReport> {
        not_implemented("search")
    }

    fn paths(&self) -> Result<FontbrewPaths> {
        match &self.paths {
            Some(paths) => Ok(paths.clone()),
            None => FontbrewPaths::resolve(),
        }
    }
}

fn not_implemented<T>(operation: &'static str) -> Result<T> {
    Err(FontbrewError::NotImplemented { operation })
}
