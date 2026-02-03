use serde::{Deserialize, Serialize};

pub mod bindings {
    wit_bindgen::generate!({ generate_all });
}

use crate::bindings::exports::betty_blocks::auth::jwt::{AuthError, AuthHeaders, Claims, Guest};

struct Component;

/// Extracts the Bearer token from the Authorization header
fn extract_token(headers: &AuthHeaders) -> Result<&str, AuthError> {
    let auth_header = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .ok_or(AuthError::MissingHeader)?;

    auth_header
        .1
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != "null")
        .ok_or(AuthError::InvalidFormat)
}

fn validate_rs256(headers: AuthHeaders) -> Result<Claims, AuthError> {
    use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};

    let token = extract_token(&headers)?;

    let header = decode_header(token).map_err(|_| AuthError::MalformedToken)?;

    if header.alg != Algorithm::RS256 {
        return Err(AuthError::UnsupportedAlgorithm(format!(
            "Expected RS256 algorithm, got: {:?}",
            header.alg
        )));
    }

    let public_key_pem = std::env::var("JWT_PUBLIC_KEY")
        .map_err(|_| AuthError::MissingConfig("JWT_PUBLIC_KEY".to_string()))?;

    let issuer = std::env::var("JWT_ISSUER")
        .map_err(|_| AuthError::MissingConfig("JWT_ISSUER".to_string()))?;

    let audience = std::env::var("JWT_AUDIENCE")
        .map_err(|_| AuthError::MissingConfig("JWT_AUDIENCE".to_string()))?;

    let mut validation = Validation::new(Algorithm::RS256);
    validation.validate_exp = true;
    validation.validate_nbf = true;
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[audience]);
    validation.leeway = 60;

    let decoding_key = DecodingKey::from_rsa_pem(public_key_pem.as_bytes())
        .map_err(|e| AuthError::InvalidPublicKey(format!("Invalid JWT public key: {}", e)))?;

    let token_data = decode::<JwtClaims>(token, &decoding_key, &validation)
        .map_err(|e| AuthError::ValidationFailed(format!("JWT validation failed: {}", e)))?;

    Ok(token_data.claims.into())
}

fn validate_hs512(headers: AuthHeaders) -> Result<Claims, AuthError> {
    use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};

    let token = extract_token(&headers)?;

    let header = decode_header(token).map_err(|_| AuthError::MalformedToken)?;

    if header.alg != Algorithm::HS512 {
        return Err(AuthError::UnsupportedAlgorithm(format!(
            "Expected HS512 algorithm, got: {:?}",
            header.alg
        )));
    }

    let secret = std::env::var("JWT_SECRET")
        .map_err(|_| AuthError::MissingConfig("JWT_SECRET".to_string()))?;

    let issuer = std::env::var("JWT_ISSUER")
        .map_err(|_| AuthError::MissingConfig("JWT_ISSUER".to_string()))?;

    let audience = std::env::var("JWT_AUDIENCE")
        .map_err(|_| AuthError::MissingConfig("JWT_AUDIENCE".to_string()))?;

    let mut validation = Validation::new(Algorithm::HS512);
    validation.validate_exp = true;
    validation.validate_nbf = true;
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[audience]);
    validation.leeway = 60;

    let decoding_key = DecodingKey::from_secret(secret.as_bytes());

    let token_data = decode::<JwtClaims>(token, &decoding_key, &validation)
        .map_err(|e| AuthError::ValidationFailed(format!("JWT validation failed: {}", e)))?;

    Ok(token_data.claims.into())
}

// Helper struct for bridging wit to native rust type
#[derive(Debug, Serialize, Deserialize)]
struct JwtClaims {
    app_uuid: String,
    aud: String,
    auth_profile: String,
    cas_token: String,
    exp: u64,
    iat: u64,
    iss: String,
    jti: String,
    locale: Option<String>,
    nbf: u64,
    roles: Vec<u32>,
    user_id: u32,
}

impl From<JwtClaims> for Claims {
    fn from(claims: JwtClaims) -> Self {
        Claims {
            app_uuid: claims.app_uuid,
            aud: claims.aud,
            auth_profile: claims.auth_profile,
            cas_token: claims.cas_token,
            exp: claims.exp,
            iat: claims.iat,
            iss: claims.iss,
            jti: claims.jti,
            locale: claims.locale,
            nbf: claims.nbf,
            roles: claims.roles,
            user_id: claims.user_id,
        }
    }
}

impl Guest for Component {
    fn validate_token(headers: AuthHeaders) -> Result<Claims, AuthError> {
        use jsonwebtoken::{decode_header, Algorithm};

        let token = extract_token(&headers)?;
        let header = decode_header(token).map_err(|_| AuthError::MalformedToken)?;

        match header.alg {
            Algorithm::RS256 => validate_rs256(headers),
            Algorithm::HS512 => validate_hs512(headers),
            alg => Err(AuthError::UnsupportedAlgorithm(format!(
                "Unsupported algorithm: {:?}. Only RS256 and HS512 are supported",
                alg
            ))),
        }
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};

    fn generate_rsa_key_pair() -> (String, String) {
        use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
        use rsa::RsaPrivateKey;

        let mut rng = rand::thread_rng();
        let private_key =
            RsaPrivateKey::new(&mut rng, 2048).expect("failed to generate private key");

        let public_key = private_key.to_public_key();

        let private_pem = private_key
            .to_pkcs8_pem(LineEnding::LF)
            .expect("failed to encode private key")
            .to_string();

        let public_pem = public_key
            .to_public_key_pem(LineEnding::LF)
            .expect("failed to encode public key")
            .to_string();

        (private_pem, public_pem)
    }

    fn generate_claims(exp_offset_seconds: i64) -> JwtClaims {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        JwtClaims {
            app_uuid: "ca432225a56f242f7bd2131da647d3c82".to_string(),
            aud: "Joken".to_string(),
            auth_profile: "5bf9eba34636495d80ed5a790ca39077".to_string(),
            cas_token: "d652585964ecfd59bd738bb33f5a421ce85c493e".to_string(),
            exp: (now as i64 + exp_offset_seconds) as u64,
            iat: now,
            iss: "Joken".to_string(),
            jti: "326h3prprbgrfr4u9k03luh2".to_string(),
            locale: None,
            nbf: now,
            roles: vec![1],
            user_id: 1,
        }
    }

    fn generate_jwt_token_rs256(private_key_pem: &str, claims: JwtClaims) -> String {
        let header = Header::new(Algorithm::RS256);
        let private_key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
            .expect("failed to load private key for signing");
        encode(&header, &claims, &private_key).expect("failed to encode JWT")
    }

    fn generate_jwt_token_hs512(secret: &[u8], claims: JwtClaims) -> String {
        let header = Header::new(Algorithm::HS512);
        encode(&header, &claims, &EncodingKey::from_secret(secret)).expect("failed to encode JWT")
    }

    fn setup_test_env_rs256(public_key: &str) {
        unsafe {
            std::env::set_var("JWT_ISSUER", "Joken");
            std::env::set_var("JWT_AUDIENCE", "Joken");
            std::env::set_var("JWT_PUBLIC_KEY", public_key);
        }
    }

    fn setup_test_env_hs512(secret: &str) {
        unsafe {
            std::env::set_var("JWT_ISSUER", "Joken");
            std::env::set_var("JWT_AUDIENCE", "Joken");
            std::env::set_var("JWT_SECRET", secret);
        }
    }

    // Tests for RS256

    #[test]
    fn test_rs256_valid_jwt_with_valid_signature() {
        let (private_key, public_key) = generate_rsa_key_pair();
        setup_test_env_rs256(&public_key);

        let claims = generate_claims(3600);
        let token = generate_jwt_token_rs256(&private_key, claims);

        let headers = vec![("Authorization".to_string(), format!("Bearer {}", token))];
        let result = Component::validate_token(headers);

        assert!(result.is_ok());
        let validated_claims = result.unwrap();
        assert_eq!(validated_claims.aud, "Joken");
        assert_eq!(validated_claims.user_id, 1);
    }

    #[test]
    fn test_rs256_valid_jwt_with_invalid_signature() {
        let (private_key1, _) = generate_rsa_key_pair();
        let (_, public_key2) = generate_rsa_key_pair();
        setup_test_env_rs256(&public_key2);

        let claims = generate_claims(3600);
        let token = generate_jwt_token_rs256(&private_key1, claims);

        let headers = vec![("Authorization".to_string(), format!("Bearer {}", token))];
        let result = Component::validate_token(headers);

        assert!(matches!(result, Err(AuthError::ValidationFailed(_))));
    }

    #[test]
    fn test_rs256_expired_jwt_token() {
        let (private_key, public_key) = generate_rsa_key_pair();
        setup_test_env_rs256(&public_key);

        let claims = generate_claims(-3600);
        let token = generate_jwt_token_rs256(&private_key, claims);

        let headers = vec![("Authorization".to_string(), format!("Bearer {}", token))];
        let result = Component::validate_token(headers);

        assert!(matches!(result, Err(AuthError::ValidationFailed(_))));
    }

    // Tests for HS512

    #[test]
    fn test_hs512_valid_jwt_with_valid_secret() {
        let secret = "super_secret_key_for_hs512_testing_purposes";
        setup_test_env_hs512(secret);

        let claims = generate_claims(3600);
        let token = generate_jwt_token_hs512(secret.as_bytes(), claims);

        let headers = vec![("Authorization".to_string(), format!("Bearer {}", token))];
        let result = Component::validate_token(headers);

        assert!(result.is_ok(), "Expected Ok, got: {:?}", result);
        let validated_claims = result.unwrap();
        assert_eq!(validated_claims.aud, "Joken");
        assert_eq!(validated_claims.user_id, 1);
    }

    #[test]
    fn test_hs512_valid_jwt_with_invalid_secret() {
        let secret1 = "correct_secret_key";
        let secret2 = "wrong_secret_key";
        setup_test_env_hs512(secret2);

        let claims = generate_claims(3600);
        let token = generate_jwt_token_hs512(secret1.as_bytes(), claims);

        let headers = vec![("Authorization".to_string(), format!("Bearer {}", token))];
        let result = Component::validate_token(headers);

        assert!(matches!(result, Err(AuthError::ValidationFailed(_))));
    }

    #[test]
    fn test_hs512_expired_jwt_token() {
        let secret = "super_secret_key_for_hs512_testing_purposes";
        setup_test_env_hs512(secret);

        let claims = generate_claims(-3600);
        let token = generate_jwt_token_hs512(secret.as_bytes(), claims);

        let headers = vec![("Authorization".to_string(), format!("Bearer {}", token))];
        let result = Component::validate_token(headers);

        assert!(matches!(result, Err(AuthError::ValidationFailed(_))));
    }

    // Common Tests

    #[test]
    fn test_malformed_jwt() {
        let (_, public_key) = generate_rsa_key_pair();
        setup_test_env_rs256(&public_key);

        let headers = vec![(
            "Authorization".to_string(),
            "Bearer not.a.valid.jwt".to_string(),
        )];
        let result = Component::validate_token(headers);

        assert!(matches!(result, Err(AuthError::MalformedToken)));
    }

    #[test]
    fn test_null_jwt_token() {
        let (_, public_key) = generate_rsa_key_pair();
        setup_test_env_rs256(&public_key);

        let headers = vec![("Authorization".to_string(), "Bearer null".to_string())];
        let result = Component::validate_token(headers);

        assert!(matches!(result, Err(AuthError::InvalidFormat)));
    }

    #[test]
    fn test_missing_authorization_header() {
        let (_, public_key) = generate_rsa_key_pair();
        setup_test_env_rs256(&public_key);

        let headers = vec![];
        let result = Component::validate_token(headers);

        assert!(matches!(result, Err(AuthError::MissingHeader)));
    }

    #[test]
    fn test_unsupported_algorithm() {
        let (_, public_key) = generate_rsa_key_pair();
        setup_test_env_rs256(&public_key);

        // Create a token with HS256 algorithm (unsupported - only RS256 and HS512 are allowed)
        let claims = generate_claims(3600);
        let secret = b"secret";
        let header = Header::new(Algorithm::HS256);
        let token = encode(&header, &claims, &EncodingKey::from_secret(secret)).unwrap();

        let headers = vec![("Authorization".to_string(), format!("Bearer {}", token))];
        let result = Component::validate_token(headers);

        assert!(matches!(result, Err(AuthError::UnsupportedAlgorithm(_))));
    }
}
