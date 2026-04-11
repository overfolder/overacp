//! JWT round-trip and rejection tests for the protocol crate.
//!
//! Ensures the `Claims` shape matches the broker's expected wire
//! format ({sub, role, user?, exp, iss}) and that validate_token
//! rejects every canonical failure mode.

use chrono::Utc;
use jsonwebtoken::{encode, EncodingKey, Header};
use overacp_protocol::jwt::{
    mint_token, peek_claims_unverified, validate_token, Claims, DEFAULT_TOKEN_TTL_SECS, ROLE_ADMIN,
    ROLE_AGENT,
};
use overacp_protocol::ProtocolError;
use uuid::Uuid;

const TEST_KEY: &str = "test-secret-key-for-jwt-signing";
const TEST_ISSUER: &str = "test-issuer";

#[test]
fn admin_roundtrip() {
    let sub = Uuid::new_v4();
    let claims = Claims::admin(sub, DEFAULT_TOKEN_TTL_SECS, TEST_ISSUER);
    let token = mint_token(TEST_KEY, &claims).unwrap();
    let decoded = validate_token(TEST_KEY, TEST_ISSUER, &token).unwrap();
    assert_eq!(decoded.sub, sub);
    assert_eq!(decoded.role, ROLE_ADMIN);
    assert!(decoded.is_admin());
    assert!(!decoded.is_agent());
    assert!(decoded.user.is_none());
}

#[test]
fn agent_roundtrip_with_user() {
    let agent_id = Uuid::new_v4();
    let user = Uuid::new_v4();
    let claims = Claims::agent(agent_id, Some(user), DEFAULT_TOKEN_TTL_SECS, TEST_ISSUER);
    let token = mint_token(TEST_KEY, &claims).unwrap();
    let decoded = validate_token(TEST_KEY, TEST_ISSUER, &token).unwrap();
    assert_eq!(decoded.sub, agent_id);
    assert_eq!(decoded.role, ROLE_AGENT);
    assert!(decoded.is_agent());
    assert_eq!(decoded.user, Some(user));
}

#[test]
fn agent_roundtrip_without_user() {
    let claims = Claims::agent(Uuid::new_v4(), None, DEFAULT_TOKEN_TTL_SECS, TEST_ISSUER);
    let token = mint_token(TEST_KEY, &claims).unwrap();
    let decoded = validate_token(TEST_KEY, TEST_ISSUER, &token).unwrap();
    assert!(decoded.user.is_none());
}

#[test]
fn wrong_key_rejected() {
    let claims = Claims::agent(Uuid::new_v4(), None, DEFAULT_TOKEN_TTL_SECS, TEST_ISSUER);
    let token = mint_token(TEST_KEY, &claims).unwrap();
    let result = validate_token("wrong-key", TEST_ISSUER, &token);
    assert!(matches!(result, Err(ProtocolError::Jwt(_))));
}

#[test]
fn expired_token_rejected() {
    // jsonwebtoken's default leeway is 60s; go well past it.
    let claims = Claims::agent(Uuid::new_v4(), None, -3600, TEST_ISSUER);
    let token = mint_token(TEST_KEY, &claims).unwrap();
    let result = validate_token(TEST_KEY, TEST_ISSUER, &token);
    assert!(matches!(result, Err(ProtocolError::Jwt(_))));
}

#[test]
fn wrong_issuer_rejected() {
    let claims = Claims::agent(Uuid::new_v4(), None, DEFAULT_TOKEN_TTL_SECS, "other-issuer");
    let token = mint_token(TEST_KEY, &claims).unwrap();
    let result = validate_token(TEST_KEY, TEST_ISSUER, &token);
    assert!(matches!(result, Err(ProtocolError::Jwt(_))));
}

#[test]
fn invalid_role_rejected() {
    // Hand-roll a token with a role that isn't admin/agent.
    let bad = Claims {
        sub: Uuid::new_v4(),
        role: "superuser".into(),
        user: None,
        exp: Utc::now().timestamp() + 60,
        iss: TEST_ISSUER.into(),
    };
    let token = encode(
        &Header::default(),
        &bad,
        &EncodingKey::from_secret(TEST_KEY.as_bytes()),
    )
    .unwrap();
    let err = validate_token(TEST_KEY, TEST_ISSUER, &token).expect_err("should reject");
    match err {
        ProtocolError::InvalidRole(role) => assert_eq!(role, "superuser"),
        other => panic!("expected InvalidRole, got {other:?}"),
    }
}

#[test]
fn peek_claims_skips_validation() {
    // A token signed with one key + issuer should still decode
    // through peek_claims_unverified, which lets the agent extract
    // `sub` before the server has authoritatively validated the
    // token.
    let agent_id = Uuid::new_v4();
    let claims = Claims::agent(agent_id, None, DEFAULT_TOKEN_TTL_SECS, "some-issuer");
    let token = mint_token("key-a", &claims).unwrap();
    let peeked = peek_claims_unverified(&token).unwrap();
    assert_eq!(peeked.sub, agent_id);
    assert_eq!(peeked.iss, "some-issuer");
    assert_eq!(peeked.role, ROLE_AGENT);
}

#[test]
fn peek_claims_survives_expired_token() {
    // Peek must not reject on expiration — its whole job is to
    // extract fields before authoritative validation.
    let claims = Claims::agent(Uuid::new_v4(), None, -3600, TEST_ISSUER);
    let token = mint_token(TEST_KEY, &claims).unwrap();
    assert!(peek_claims_unverified(&token).is_ok());
}

#[test]
fn peek_claims_rejects_malformed_token() {
    assert!(peek_claims_unverified("not.a.jwt").is_err());
    assert!(peek_claims_unverified("garbage").is_err());
}
