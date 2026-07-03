use crate::error::{FontbrewError, Result};
use crate::model::{
    CancellationToken, ExecutionPolicy, InfoReport, InfoRequest, InstallPlan, InstallReport,
    InstallRequest, ListReport, OutdatedReport, OutdatedRequest, ProgressSink, RemovePlan,
    RemoveReport, RemoveRequest, SearchReport, SearchRequest, UpdatePlan, UpdateReport,
    UpdateRequest,
};

#[derive(Debug, Default, Clone)]
pub struct FontbrewApp;

impl FontbrewApp {
    pub fn new() -> Self {
        Self
    }

    pub fn install_plan(&self, _request: InstallRequest) -> Result<InstallPlan> {
        not_implemented("install_plan")
    }

    pub fn apply_install(
        &self,
        _plan: InstallPlan,
        _policy: ExecutionPolicy,
        _progress: &mut dyn ProgressSink,
        _cancellation: &dyn CancellationToken,
    ) -> Result<InstallReport> {
        not_implemented("apply_install")
    }

    pub fn list_packages(&self) -> Result<ListReport> {
        not_implemented("list_packages")
    }

    pub fn package_info(&self, _request: InfoRequest) -> Result<InfoReport> {
        not_implemented("package_info")
    }

    pub fn remove_plan(&self, _request: RemoveRequest) -> Result<RemovePlan> {
        not_implemented("remove_plan")
    }

    pub fn apply_remove(
        &self,
        _plan: RemovePlan,
        _policy: ExecutionPolicy,
        _progress: &mut dyn ProgressSink,
        _cancellation: &dyn CancellationToken,
    ) -> Result<RemoveReport> {
        not_implemented("apply_remove")
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
}

fn not_implemented<T>(operation: &'static str) -> Result<T> {
    Err(FontbrewError::NotImplemented { operation })
}
