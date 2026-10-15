use super::*;
use pretty_assertions::assert_eq;

#[test]
fn supplied_token_is_stored_without_remote_metadata() {
    let auth = PersonalAccessTokenAuth::from_supplied_token("at-example");

    assert_eq!(auth.access_token(), "at-example");
    assert_eq!(auth.account_id(), "");
    assert_eq!(auth.chatgpt_user_id(), "");
    assert_eq!(auth.email(), None);
    assert_eq!(auth.plan_type(), AccountPlanType::Unknown);
    assert!(!auth.is_fedramp_account());
}
