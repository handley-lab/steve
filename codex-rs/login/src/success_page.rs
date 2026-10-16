use url::Url;

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub enum LoginSuccessPage {
    #[default]
    Local,
    Hosted {
        url: Url,
        app_brand: LoginSuccessPageBrand,
    },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LoginSuccessPageBrand {
    Codex,
    Chatgpt,
}
