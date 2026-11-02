//! The socket-control **password store** (M4.2) — the port of upstream's
//! `SocketControlPasswordStore` (`cmux-main/Packages/macOS/CmuxSettings/...
//! SocketControl/SocketControlPasswordStore.swift`) with the Windows
//! credential-analog choice ratified in ADR-0005 Amendment 3: a
//! DPAPI-protected file instead of a plaintext 0600 file.
//!
//! Password sources, highest priority first (upstream order):
//! 1. the explicit value (CLI `--password` favorite),
//! 2. the `CMUX_SOCKET_PASSWORD` environment variable (`socketPasswordEnvKey`),
//! 3. the password file (upstream: `<state>/socket-control-password`, 0600;
//!    port: `%APPDATA%\cmux\socket-control-password`, DPAPI user-scope blob,
//!    relocatable via `CMUX_PASSWORD_FILE`).
//!
//! The macOS Keychain (upstream's legacy third source, migrated into the
//! file long ago) has no Windows analog to carry over — the DPAPI blob *is*
//! the stored credential. Any failure to read or decrypt the file counts as
//! "unconfigured", exactly like upstream's swallowed `(try?
//! loadPassword())`: password mode then answers `auth_unconfigured`, never a
//! half-verified yes.
//!
//! Verification is a full-byte constant-time compare (upstream
//! `constantTimeEquals`), and rotation is live: callers re-read through
//! [`configured_password`] for every check, so env/file edits take effect on
//! the next attempt or revoke an authenticated connection (upstream
//! `SocketPasswordAuthorization`).

use std::io;
use std::path::{Path, PathBuf};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{LocalFree, HLOCAL, NTSTATUS};
use windows::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash,
    BCryptHashData, BCryptOpenAlgorithmProvider, CryptProtectData, CryptUnprotectData,
    BCRYPT_ALG_HANDLE, BCRYPT_HASH_HANDLE, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS,
    CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

/// Environment key upstream types the password into (`socketPasswordEnvKey`).
pub const PASSWORD_ENV_KEY: &str = "CMUX_SOCKET_PASSWORD";

/// Environment override for the password file location (smoke affordance,
/// mirroring `CMUX_SESSION_FILE`; upstream has no equivalent override).
pub const PASSWORD_FILE_ENV_KEY: &str = "CMUX_PASSWORD_FILE";

/// Password file name (upstream `SocketControlPasswordStore.fileName`).
pub const PASSWORD_FILE_NAME: &str = "socket-control-password";

/// Default password file location: the app-state directory
/// (`%APPDATA%\cmux\socket-control-password`). Upstream deliberately keeps
/// the file in the state directory — not Application Support — so separately
/// signed helper processes can read it; on Windows `%APPDATA%\cmux` is where
/// the session store already lives, and the profile ACLs deny other users.
pub fn default_password_file_path() -> Option<PathBuf> {
    if let Some(override_path) = std::env::var_os(PASSWORD_FILE_ENV_KEY) {
        let trimmed = override_path.to_string_lossy().trim().to_owned();
        if trimmed.is_empty() {
            return None;
        }
        return Some(PathBuf::from(trimmed));
    }
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(&appdata)
            .join("cmux")
            .join(PASSWORD_FILE_NAME),
    )
}

/// Resolves the configured password, highest priority first. `explicit` is
/// the CLI `--password` favorite; `getenv` supplies environment reads (real
/// callers pass the process environment, tests inject a map); `file_path`
/// points at the password file — `None` disables the file source entirely,
/// while `Some(missing)` reads as absent.
pub fn resolve_with(
    explicit: Option<&str>,
    getenv: &dyn Fn(&str) -> Option<String>,
    file_path: Option<&Path>,
) -> Option<String> {
    if let Some(password) = normalized(explicit) {
        return Some(password);
    }
    if let Some(from_env) = normalized(getenv(PASSWORD_ENV_KEY).as_deref()) {
        return Some(from_env);
    }
    if let Some(file_path) = file_path {
        if let Some(from_file) = read_protected_file(file_path) {
            return Some(from_file);
        }
    }
    None
}

/// The production resolver: explicit value, or the real environment, or the
/// default (override-aware) password file. Re-runs on every call so rotation
/// is live (upstream reads the store per verification).
pub fn configured_password(explicit: Option<&str>) -> Option<String> {
    let file = default_password_file_path();
    resolve_with(explicit, &|key| std::env::var(key).ok(), file.as_deref())
}

/// Normalizes a candidate: trims surrounding whitespace/newlines; a value
/// that trims away is *unconfigured*, never an empty password (upstream
/// `normalized`).
pub fn normalized(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_owned())
}

/// Constant-time compare over the full UTF-8 bytes of both strings (upstream
/// `constantTimeEquals`): folds the length difference and every byte pair
/// into one difference cell — no early exit.
pub fn constant_time_equals(expected: &str, candidate: &str) -> bool {
    constant_time_bytes(expected.as_bytes(), candidate.as_bytes())
}

/// Constant-time compare over byte slices, independent of length.
pub fn constant_time_bytes(expected: &[u8], candidate: &[u8]) -> bool {
    // Same shape as upstream's string variant: XOR of lengths, then every
    // byte of the expected slice folded in, indexing the candidate safely.
    let mut difference = expected.len() ^ candidate.len();
    for (index, expected_byte) in expected.iter().copied().enumerate() {
        let candidate_byte = candidate.get(index).copied().unwrap_or(0);
        difference |= usize::from(expected_byte ^ candidate_byte);
    }
    difference == 0
}
/// SHA-256 via CNG (`bcrypt.dll`) — the connection-rotation fingerprint
/// (upstream `SocketPasswordAuthorization.fingerprint`).
pub fn password_fingerprint(password: &str) -> [u8; 32] {
    sha256(password.as_bytes())
}

/// SHA-256 over arbitrary bytes (CNG, zero new crates).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    unsafe {
        let mut algorithm = BCRYPT_ALG_HANDLE::default();
        let status = BCryptOpenAlgorithmProvider(
            &mut algorithm,
            w!("SHA256"),
            PCWSTR::null(),
            BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
        );
        if status != NTSTATUS(0) {
            panic!("BCryptOpenAlgorithmProvider failed: {status:?}");
        }
        let mut hash = BCRYPT_HASH_HANDLE::default();
        let status = BCryptCreateHash(algorithm, &mut hash, None, None, 0);
        if status != NTSTATUS(0) {
            panic!("BCryptCreateHash failed: {status:?}");
        }
        let status = BCryptHashData(hash, data, 0);
        if status != NTSTATUS(0) {
            panic!("BCryptHashData failed: {status:?}");
        }
        let mut digest = [0u8; 32];
        let status = BCryptFinishHash(hash, &mut digest, 0);
        if status != NTSTATUS(0) {
            panic!("BCryptFinishHash failed: {status:?}");
        }
        let _ = BCryptDestroyHash(hash);
        let _ = BCryptCloseAlgorithmProvider(algorithm, 0);
        digest
    }
}

/// Encrypts `plaintext` with user-scoped DPAPI and stores it at `path`
/// (creating the directory). At-rest protection: the current Windows user.
pub fn write_protected_file(plaintext: &str, path: &Path) -> io::Result<()> {
    let blob = dpapi_protect(plaintext.as_bytes())
        .map_err(|failure| io::Error::other(format!("CryptProtectData failed: {failure}")))?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, &blob)
}

/// Removes the password file, if present (upstream `clearPassword`).
pub fn clear_password_file(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(failure) if failure.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(failure) => Err(failure),
    }
}

/// Reads and decrypts the stored password. Absent, corrupt, undecryptable,
/// or un-decodable all read as "not configured" — swallowing failures the
/// way upstream's `(try? loadPassword()) ?? nil` does. Empty after trim is
/// also unconfigured.
pub fn read_protected_file(path: &Path) -> Option<String> {
    let blob = std::fs::read(path).ok()?;
    let plaintext = dpapi_unprotect(&blob)
        .ok()
        .flatten()
        .and_then(|bytes| String::from_utf8(bytes).ok());
    normalized(plaintext.as_deref())
}

/// DPAPI protect (`crypt32 CryptProtectData`, UI-forbidden, no extra
/// entropy — upstream parity: no combiner).
fn dpapi_protect(plaintext: &[u8]) -> windows::core::Result<Vec<u8>> {
    unsafe {
        let input = blob_for(plaintext);
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(
            &input,
            w!("cmux.socket-control-password.v1"),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )?;
        Ok(collect_blob(&mut output))
    }
}

/// DPAPI unprotect counterpart.
fn dpapi_unprotect(blob: &[u8]) -> windows::core::Result<Option<Vec<u8>>> {
    unsafe {
        let input = blob_for(blob);
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )?;
        Ok(Some(collect_blob(&mut output)))
    }
}

fn blob_for(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    }
}

/// Copies out and frees the DPAPI result blob (crypt32 hands back a
/// `LocalAlloc`'d buffer; the crate's `LocalFree` idiom from `sddl.rs`).
unsafe fn collect_blob(output: &mut CRYPT_INTEGER_BLOB) -> Vec<u8> {
    let slice = std::slice::from_raw_parts(output.pbData as *const u8, output.cbData as usize);
    let bytes = slice.to_vec();
    if !output.pbData.is_null() {
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
    }
    output.pbData = std::ptr::null_mut();
    output.cbData = 0;
    bytes
}

/// Auth state carried by one connection in password mode (upstream
/// `SocketPasswordAuthorization`): the SHA-256 fingerprint of the *verified*
/// password. Unauthenticated connections stay connected to attempt login
/// (and see real errors); authenticated ones are re-verified against the
/// live store before every served command.
#[derive(Clone, Copy)]
pub struct PasswordAuthorization {
    fingerprint: Option<[u8; 32]>,
}

impl PasswordAuthorization {
    /// Fresh connection; may attempt login and nothing else.
    pub const fn new() -> Self {
        Self { fingerprint: None }
    }

    /// Whether a login handshake has succeeded on this connection.
    pub fn is_authenticated(&self) -> bool {
        self.fingerprint.is_some()
    }

    /// Binds the connection to the password that just passed verification
    /// (upstream `authenticate(password:)`).
    pub fn authenticate(&mut self, password: &str) {
        self.fingerprint = Some(password_fingerprint(password));
    }

    /// Whether an already-authenticated connection may continue: only while
    /// the *current* store fingerprint matches. `current: None` (store
    /// emptied) revokes — upstream `permitsConnectionContinuation`.
    /// Unauthenticated always answers yes so a login remains possible.
    pub fn permits_continuation(&self, current: Option<&str>) -> bool {
        let Some(fingerprint) = self.fingerprint else {
            return true;
        };
        let Some(current) = normalized(current) else {
            return false;
        };
        constant_time_bytes(&fingerprint, &password_fingerprint(&current))
    }
}

impl Default for PasswordAuthorization {
    fn default() -> Self {
        Self::new()
    }
}

/// Type used by tests to inject a fake environment (also readable in
/// signatures instead of the raw boxed-closure spelling).
pub type PasswordLookup<'a> = Box<dyn Fn(&str) -> Option<String> + 'a>;

#[cfg(test)]
mod tests {
    use super::*;

    fn env_lookup(entries: &[(&str, &str)]) -> PasswordLookup<'static> {
        let owned: Vec<(String, String)> = entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        Box::new(move |key: &str| {
            owned
                .iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value.clone())
        })
    }

    #[test]
    fn normalize_trims_and_rejects_blanks() {
        assert_eq!(normalized(Some("  secret \n")), Some("secret".to_owned()));
        assert_eq!(normalized(Some("   ")), None);
        assert_eq!(normalized(None), None);
    }

    #[test]
    fn resolution_precedence_is_explicit_env_then_file() {
        let env = env_lookup(&[(PASSWORD_ENV_KEY, "from-env")]);
        let explicit = resolve_with(Some("explicit"), &env, None);
        assert_eq!(explicit.as_deref(), Some("explicit"));

        let env_only = resolve_with(None, &env, None);
        assert_eq!(env_only.as_deref(), Some("from-env"));

        // A blank env value does not carry precedence over the file source.
        let blank_env = env_lookup(&[(PASSWORD_ENV_KEY, "   ")]);
        assert_eq!(resolve_with(None, &blank_env, None), None);

        // No source at all reads as unconfigured.
        let nothing = env_lookup(&[]);
        assert_eq!(resolve_with(None, &nothing, None), None);
    }

    #[test]
    fn explicit_blank_falls_through_instead_of_matching_blank() {
        let env = env_lookup(&[(PASSWORD_ENV_KEY, "from-env")]);
        let resolved = resolve_with(Some(" "), &env, None);
        assert_eq!(resolved.as_deref(), Some("from-env"));
    }

    #[test]
    fn constant_time_compare_is_exact() {
        assert!(constant_time_equals("secret", "secret"));
        assert!(!constant_time_equals("secret", "secrets"));
        assert!(!constant_time_equals("secret", "secretx"));
        assert!(!constant_time_equals("sec", "a longer candidate"));
        assert!(constant_time_bytes(&[], &[]));
        assert!(!constant_time_bytes(&[1u8, 2], &[1u8, 3]));
    }

    #[test]
    fn fingerprint_matches_sha256() {
        // NIST FIPS 180 test vector for SHA-256("abc").
        let expected = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(password_fingerprint("abc"), expected);
    }

    #[test]
    fn dpapi_round_trip_returns_the_exact_plaintext() {
        let secret = "round-trip-secret";
        let protected = dpapi_protect(secret.as_bytes())
            .expect("DPAPI protect must work for the interactive test user");
        assert!(!protected.is_empty());
        // The blob must not contain the plaintext fragment.
        let secret_bytes = secret.as_bytes();
        assert!(!protected
            .windows(secret_bytes.len())
            .any(|w| w == secret_bytes));

        let unprotected = dpapi_unprotect(&protected)
            .expect("DPAPI unprotect must succeed for this user")
            .expect("descriptor present");
        assert_eq!(unprotected.as_slice(), secret_bytes);

        // A corrupted blob fails unprotect, never half-decrypts: the last
        // byte may coincide with padding, so only assert that a success
        // never returns the plaintext.
        let mut corrupted = protected.clone();
        let last = corrupted.len() - 1;
        corrupted[last] ^= 0x5a;
        if let Ok(Some(payload)) = dpapi_unprotect(&corrupted) {
            assert_ne!(payload.as_slice(), secret_bytes);
        }
    }

    #[test]
    fn protected_file_round_trip_reads_back_the_password() {
        let mut path = std::env::temp_dir();
        path.push(format!("cmux-ipc-m42-roundtrip-{}.bin", std::process::id()));
        let _ = std::fs::remove_file(&path);

        write_protected_file("  session  \n", &path)
            .expect("a writable temp path must accept the protected file");
        assert_eq!(read_protected_file(&path).as_deref(), Some("session"));

        // Absent after clear; clearing an absent file is a no-op success.
        std::fs::remove_file(&path).ok();
        assert_eq!(read_protected_file(&path), None);
        clear_password_file(&path).expect("clear must tolerate absent files");
    }

    #[test]
    fn authorization_rotation_semantics_match_upstream() {
        let mut authorization = PasswordAuthorization::new();
        assert!(!authorization.is_authenticated());
        // Unauthenticated connections always continue (they may login).
        assert!(authorization.permits_continuation(None));
        assert!(authorization.permits_continuation(Some("never checked yet")));

        authorization.authenticate("secret");
        assert!(authorization.is_authenticated());
        assert!(authorization.permits_continuation(Some("secret")));
        assert!(authorization.permits_continuation(Some(" secret ")));
        assert!(!authorization.permits_continuation(Some("other")));
        assert!(!authorization.permits_continuation(None));
    }
}
