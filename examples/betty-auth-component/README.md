# Betty Blocks JWT Authentication Component

A reusable webassembly component that provides JWT authentication functionality using the Component Model and WIT interfaces.

## Overview

This component isolates JWT authentication logic into a separate, reusable WIT interface that can be imported by other components. It validates RS256 signed JWT tokens and returns structured claims data.

## Features

- RS256 Algorithm Only: Enforces RSA 256 signature validation for security
- Environment-based Configuration: Reads JWT public key, issuer, and audience from environment variables
- complete validation: Validates token signature, expiration, not-before, issuer, and audience
- expansive error handling: provides specific error types for different authentication failures

## WIT Interface

The component exports a `jwt` interface with a single function:

```wit
validate-token: func(headers: list<tuple<string, string>>) -> result<claims, auth-error>
```

### standard claims structure

```wit
record claims {
    app-uuid: string,
    aud: string,
    auth-profile: string,
    cas-token: string,
    exp: u64,
    iat: u64,
    iss: string,
    jti: string,
    locale: option<string>,
    nbf: u64,
    roles: list<u32>,
    user-id: u32,
}
```

### Error Types

```wit
variant auth-error {
    missing-header,
    invalid-format,
    malformed-token,
    unsupported-algorithm(string),
    missing-config(string),
    invalid-public-key(string),
    validation-failed(string),
}
```

## Environment Variables

The component requires three environment variables to be set: (do that using `std::env::var`)

| Variable | Description | Example |
|----------|-------------|---------|
| `JWT_ISSUER` | Expected token issuer | `"Joken"` |
| `JWT_AUDIENCE` | Expected token audience | `"Joken"` |
| `JWT_PUBLIC_KEY` | RSA public key in PEM format | `"-----BEGIN PUBLIC KEY-----\n..."` |

## Usage

### 1. Build the auth component

```bash
wash build
```

### 2. Import in your component

add the wit import to your component's wit file:

```wit
package example:namespace;

world testing {
    import betty-blocks:auth/jwt;
    export http:handler/incoming-handler;
}
```

### 3. How to use in Your Code

```rust
use crate::bindings::betty_blocks::auth::jwt::{validate_token, Claims, AuthError, AuthHeaders};

// extract ze headers
let headers: AuthHeaders = request
            .headers()
            .iter()
            .filter_map(|(k, v)| {
                let v_str = String::from_utf8_lossy(v.as_bytes()).to_string();
                if v_str.trim().is_empty() {
                    None
                } else {
                    Some((k.to_string(), v_str))
                }
            })
            .collect();

// Validate the JWT
match validate_token(headers) {
    Ok(claims) => {
        // Token is valid, use claims for role authorization etc.
        // or route request / call protected func here
    }
    Err(auth_error) => {
        // Handle auth err
        match auth_error {
            AuthError::MissingHeader => { }
            AuthError::InvalidFormat => { }
            AuthError::ValidationFailed(msg) => { }
            // ... handle other errors
        }
    }
}
```

## Security

1. Algorithm Whitelist: will only accept RS256 tokens, & reject symmetric algorithms (this is up for discussion depending on what algo we were using before)
2. Time validation: validates both exp (expiration) and nbf (not-before) claims with 60-second leeway/leniency
3. Issuer/Audience validation: verifies tokens are issued by expected source for expected audience
4. Malformed token detection: will reject malformed, null, or empty tokens
5. Public key validation: ensures RSA public key is properly formatted

## Testing

Run tests with:

```bash
cargo test
```