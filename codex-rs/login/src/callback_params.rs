#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginOnboardingEntrypoint {
    LifeSciences,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoginCallbackResult {
    pub onboarding_entrypoint: Option<LoginOnboardingEntrypoint>,
}
