//! JWT round-trip and rejection tests, lifted from
//! `overfolder/controlplane/src/session.rs:74-153` and parameterized
//! over the issuer string.

use jsonwebtoken::{encode, EncodingKey, Header};
use overacp_protocol::jwt::{
    mint_token, peek_claims_unverified, validate_token, Claims, DEFAULT_TOKEN_TTL_SECS,
};
use uuid::Uuid;

const TEST_KEY: &str = "test-secret-key-for-jwt-signing";
const TEST_ISSUER: &str = "test-issuer";

#[test]
fn mint_and_validate_roundtrip() {
    let agent_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let conv_id = Uuid::new_v4();

    let token = mint_token(
        TEST_KEY,
        TEST_ISSUER,
        DEFAULT_TOKEN_TTL_SECS,
        agent_id,
        user_id,
        conv_id,
    )
    .unwrap();
    let claims = validate_token(TEST_KEY, TEST_ISSUER, &token).unwrap();

    assert_eq!(claims.sub, agent_id);
    assert_eq!(claims.user, user_id);
    assert_eq!(claims.conv, conv_id);
    assert_eq!(claims.iss, TEST_ISSUER);
}

#[test]
fn wrong_key_rejected() {
    let token = mint_token(
        TEST_KEY,
        TEST_ISSUER,
        DEFAULT_TOKEN_TTL_SECS,
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    )
    .unwrap();
    let result = validate_token("wrong-key", TEST_ISSUER, &token);
    assert!(result.is_err());
}

#[test]
fn expired_token_rejected() {
    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        sub: Uuid::new_v4(),
        user: Uuid::new_v4(),
        conv: Uuid::new_v4(),
        exp: now - 100,
        iss: TEST_ISSUER.to_string(),
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(TEST_KEY.as_bytes()),
    )
    .unwrap();

    let result = validate_token(TEST_KEY, TEST_ISSUER, &token);
    assert!(result.is_err());
}

#[test]
fn wrong_issuer_rejected() {
    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        sub: Uuid::new_v4(),
        user: Uuid::new_v4(),
        conv: Uuid::new_v4(),
        exp: now + 3600,
        iss: "not-the-expected-issuer".to_string(),
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(TEST_KEY.as_bytes()),
    )
    .unwrap();

    let result = validate_token(TEST_KEY, TEST_ISSUER, &token);
    assert!(result.is_err());
}

#[test]
fn peek_claims_skips_validation() {
    // A token signed with one key + issuer should still decode
    // through peek_claims_unverified, which lets the agent extract
    // `conv` before the server has authoritatively validated the token.
    let conv_id = Uuid::new_v4();
    let token = mint_token(
        "key-a",
        "issuer-a",
        DEFAULT_TOKEN_TTL_SECS,
        Uuid::new_v4(),
        Uuid::new_v4(),
        conv_id,
    )
    .unwrap();

    let claims = peek_claims_unverified(&token).unwrap();
    assert_eq!(claims.conv, conv_id);
    assert_eq!(claims.iss, "issuer-a");
}
