//! A minimal native TPM 2.0 client: the commands sealing needs, sessions
//! salted to the storage key by ECDH with AES-128-CFB parameter encryption,
//! and response HMACs checked before anything in a response is used.

use aws_lc_rs::agreement::{ECDH_P256, EphemeralPrivateKey, UnparsedPublicKey, agree_ephemeral};
use aws_lc_rs::cipher::{
    AES_128, DecryptingKey, DecryptionContext, EncryptingKey, EncryptionContext, UnboundCipherKey,
};
use aws_lc_rs::hmac;
use aws_lc_rs::iv::FixedLength;
use aws_lc_rs::rand::SystemRandom;
use zeroize::Zeroizing;

use super::wire::{R, Res, W};

const ST_NO_SESSIONS: u16 = 0x8001;
const ST_SESSIONS: u16 = 0x8002;

pub const CC_CREATE_PRIMARY: u32 = 0x131;
pub const CC_CREATE: u32 = 0x153;
pub const CC_LOAD: u32 = 0x157;
pub const CC_UNSEAL: u32 = 0x15E;
pub const CC_FLUSH_CONTEXT: u32 = 0x165;
pub const CC_START_AUTH_SESSION: u32 = 0x176;
pub const CC_PCR_READ: u32 = 0x17E;
pub const CC_POLICY_PCR: u32 = 0x17F;
pub const CC_PCR_EXTEND: u32 = 0x182;

const RH_OWNER: u32 = 0x4000_0001;
const RH_NULL: u32 = 0x4000_0007;
const RS_PW: u32 = 0x4000_0009;

const ALG_SHA256: u16 = 0x000B;
const ALG_AES: u16 = 0x0006;
const ALG_CFB: u16 = 0x0043;
const ALG_NULL: u16 = 0x0010;
const ALG_ECC: u16 = 0x0023;
const ALG_KEYEDHASH: u16 = 0x0008;
const ECC_NIST_P256: u16 = 0x0003;

const SE_HMAC: u8 = 0x00;
const SE_POLICY: u8 = 0x01;
const ATTR_DECRYPT: u8 = 0x20;
const ATTR_ENCRYPT: u8 = 0x40;

/// The largest response read from the TPM.
const MAX_RESPONSE: usize = 4096;

/// The PCRs phase A binds: 7 (Secure Boot policy, db/dbx) and 15 (zero
/// until the runkit extends it, see [`super::SEALED_OFF`]).
pub const POLICY_PCRS: [u32; 2] = [7, 15];

/// One command in, one response out: /dev/tpmrm0 on a device, a socket or a
/// fake in tests.
pub trait Transport {
    fn transmit(&mut self, command: &[u8]) -> Res<Vec<u8>>;
}

impl Transport for std::fs::File {
    fn transmit(&mut self, command: &[u8]) -> Res<Vec<u8>> {
        use std::io::{Read, Write};
        self.write_all(command)
            .map_err(|e| format!("TPM write: {e}"))?;
        let mut out = vec![0u8; MAX_RESPONSE];
        let n = self.read(&mut out).map_err(|e| format!("TPM read: {e}"))?;
        out.truncate(n);
        Ok(out)
    }
}

/// A TPM error code, kept apart from transport and parse failures.
#[derive(Debug)]
pub enum Failure {
    Tpm(u32),
    Other(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tpm(rc) => write!(f, "TPM rc 0x{rc:03x}{}", rc_name(*rc)),
            Self::Other(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for Failure {}

impl From<String> for Failure {
    fn from(s: String) -> Self {
        Self::Other(s)
    }
}

impl From<&str> for Failure {
    fn from(s: &str) -> Self {
        Self::Other(s.to_string())
    }
}

fn rc_name(rc: u32) -> &'static str {
    // Format-one codes carry the session/handle index in bits 8..11.
    if rc & 0x80 == 0 {
        return "";
    }
    match rc & 0xBF {
        0x9D => " (TPM_RC_POLICY_FAIL)",
        0x84 => " (TPM_RC_VALUE)",
        0x8E => " (TPM_RC_AUTH_FAIL)",
        0x9B => " (TPM_RC_INTEGRITY)",
        0xA2 => " (TPM_RC_BAD_AUTH)",
        _ => "",
    }
}

type Out<T> = Result<T, Failure>;

pub fn sha256(parts: &[&[u8]]) -> Vec<u8> {
    let mut context = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA256);
    for part in parts {
        context.update(part);
    }
    context.finish().as_ref().to_vec()
}

fn hmac_sign(key: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let mut context = hmac::Context::with_key(&hmac::Key::new(hmac::HMAC_SHA256, key));
    for part in parts {
        context.update(part);
    }
    context.sign().as_ref().to_vec()
}

/// KDFa (TPM 2.0 Part 1, 11.4.10.2).
pub fn kdfa(key: &[u8], label: &str, u: &[u8], v: &[u8], bits: u32) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(Vec::new());
    let mut counter = 1u32;
    while out.len() * 8 < bits as usize {
        out.extend(hmac_sign(
            key,
            &[
                &counter.to_be_bytes(),
                label.as_bytes(),
                &[0],
                u,
                v,
                &bits.to_be_bytes(),
            ],
        ));
        counter += 1;
    }
    out.truncate(bits as usize / 8);
    out
}

/// KDFe (TPM 2.0 Part 1, 11.4.10.3).
pub fn kdfe(z: &[u8], label: &str, u: &[u8], v: &[u8], bits: u32) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(Vec::new());
    let mut counter = 1u32;
    while out.len() * 8 < bits as usize {
        out.extend(sha256(&[
            &counter.to_be_bytes(),
            z,
            label.as_bytes(),
            &[0],
            u,
            v,
        ]));
        counter += 1;
    }
    out.truncate(bits as usize / 8);
    out
}

/// AES-128-CFB over `data` with the 32 bytes of KDFa "CFB" (key, then IV).
pub fn cfb(key_iv: &[u8], data: &mut [u8], encrypt: bool) -> Out<()> {
    if data.is_empty() {
        return Ok(());
    }
    let (key, iv) = key_iv.split_at(16);
    let iv: [u8; 16] = iv.try_into().map_err(|_| "CFB IV")?;
    let key = || UnboundCipherKey::new(&AES_128, key).map_err(|_| "CFB key");
    let done = if encrypt {
        EncryptingKey::cfb128(key()?)
            .and_then(|k| {
                k.less_safe_encrypt(data, EncryptionContext::Iv128(FixedLength::from(iv)))
            })
            .map(drop)
    } else {
        DecryptingKey::cfb128(key()?)
            .and_then(|k| k.decrypt(data, DecryptionContext::Iv128(FixedLength::from(iv))))
            .map(drop)
    };
    done.map_err(|_| Failure::from("AES-128-CFB"))
}

pub fn random(n: usize) -> Out<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(vec![0u8; n]);
    aws_lc_rs::rand::fill(&mut bytes).map_err(|_| "random")?;
    Ok(bytes)
}

/// The PCR selection of `pcrs` in the SHA-256 bank.
pub fn pcr_selection(pcrs: &[u32]) -> Vec<u8> {
    let mut select = [0u8; 3];
    for pcr in pcrs {
        select[(*pcr / 8) as usize] |= 1 << (pcr % 8);
    }
    let mut w = W::default();
    w.u32(1).u16(ALG_SHA256).u8(3).raw(&select);
    w.0
}

/// The policy digest of one TPM2_PolicyPCR over [`POLICY_PCRS`] holding
/// `values` (in selection order), computed without a TPM.
pub fn policy_pcr_digest(values: &[Vec<u8>]) -> Vec<u8> {
    let parts: Vec<&[u8]> = values.iter().map(Vec::as_slice).collect();
    let pcr_digest = sha256(&parts);
    sha256(&[
        &[0u8; 32],
        &CC_POLICY_PCR.to_be_bytes(),
        &pcr_selection(&POLICY_PCRS),
        &pcr_digest,
    ])
}

/// A parsed response.
pub struct Response {
    pub handles: Vec<u32>,
    pub params: Vec<u8>,
    /// nonceTPM, attributes and HMAC of each session, in order.
    pub auths: Vec<(Vec<u8>, u8, Vec<u8>)>,
}

/// Parse a response that answers `n_handles` handles; its tag says whether
/// it carries a parameter size and sessions.
pub fn parse_response(bytes: &[u8], n_handles: usize) -> Out<Response> {
    let mut r = R::new(bytes);
    let tag = r.u16()?;
    let size = r.u32()? as usize;
    if size != bytes.len() {
        return Err(format!("response size {size}, {} read", bytes.len()).into());
    }
    let rc = r.u32()?;
    if rc != 0 {
        r.done()?;
        return Err(Failure::Tpm(rc));
    }
    let mut handles = Vec::new();
    for _ in 0..n_handles {
        handles.push(r.u32()?);
    }
    match tag {
        ST_NO_SESSIONS => Ok(Response {
            handles,
            params: r.rest().to_vec(),
            auths: Vec::new(),
        }),
        ST_SESSIONS => {
            let n = r.u32()? as usize;
            let params = r.take(n)?.to_vec();
            let mut auths = Vec::new();
            while r.pos() < bytes.len() {
                let nonce = r.b()?.to_vec();
                let attrs = r.u8()?;
                let mac = r.b()?.to_vec();
                auths.push((nonce, attrs, mac));
            }
            Ok(Response {
                handles,
                params,
                auths,
            })
        }
        other => Err(format!("response tag 0x{other:04x}").into()),
    }
}

/// An ECC public point as the TPM reports it.
pub struct EccPublic {
    pub x: Vec<u8>,
    pub y: Vec<u8>,
}

/// The point of a TPMT_PUBLIC that is a P-256 key.
pub fn parse_ecc_public(area: &[u8]) -> Res<EccPublic> {
    let mut r = R::new(area);
    if r.u16()? != ALG_ECC {
        return Err("the primary is not an ECC key".into());
    }
    let _name_alg = r.u16()?;
    let _attrs = r.u32()?;
    let _policy = r.b()?;
    if r.u16()? != ALG_NULL {
        let _bits = r.u16()?;
        let _mode = r.u16()?;
    }
    if r.u16()? != ALG_NULL {
        let _hash = r.u16()?;
    }
    if r.u16()? != ECC_NIST_P256 {
        return Err("the primary is not on P-256".into());
    }
    if r.u16()? != ALG_NULL {
        let _hash = r.u16()?;
    }
    let x = r.b()?.to_vec();
    let y = r.b()?.to_vec();
    r.done()?;
    Ok(EccPublic { x, y })
}

/// CreatePrimary's response body: (outPublic area, name).
pub fn parse_create_primary(params: &[u8]) -> Res<(Vec<u8>, Vec<u8>)> {
    let mut r = R::new(params);
    let public = r.b()?.to_vec();
    let _creation_data = r.b()?;
    let _creation_hash = r.b()?;
    let _ticket_tag = r.u16()?;
    let _ticket_hierarchy = r.u32()?;
    let _ticket_digest = r.b()?;
    let name = r.b()?.to_vec();
    r.done()?;
    Ok((public, name))
}

/// Create's response body: (outPrivate, outPublic).
pub fn parse_create(params: &[u8]) -> Res<(Vec<u8>, Vec<u8>)> {
    let mut r = R::new(params);
    let private = r.b()?.to_vec();
    let public = r.b()?.to_vec();
    let _creation_data = r.b()?;
    let _creation_hash = r.b()?;
    let _ticket_tag = r.u16()?;
    let _ticket_hierarchy = r.u32()?;
    let _ticket_digest = r.b()?;
    r.done()?;
    Ok((private, public))
}

/// The values of a PCR_Read response.
pub fn parse_pcr_read(params: &[u8]) -> Res<Vec<Vec<u8>>> {
    let mut r = R::new(params);
    let _counter = r.u32()?;
    let banks = r.u32()?;
    for _ in 0..banks {
        let _alg = r.u16()?;
        let n = r.u8()? as usize;
        let _select = r.take(n)?;
    }
    let count = r.u32()?;
    let mut values = Vec::new();
    for _ in 0..count {
        values.push(r.b()?.to_vec());
    }
    r.done()?;
    Ok(values)
}

/// A salted session, as the caller tracks it.
pub struct Session {
    pub handle: u32,
    key: Zeroizing<Vec<u8>>,
    nonce_tpm: Vec<u8>,
}

/// The storage primary, recreated from the SRK template.
pub struct Primary {
    pub handle: u32,
    pub public: EccPublic,
    pub name: Vec<u8>,
}

pub struct Client<T> {
    transport: T,
}

impl<T: Transport> Client<T> {
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }

    #[cfg(test)]
    pub const fn transport(&self) -> &T {
        &self.transport
    }

    #[cfg(test)]
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    fn command(
        &mut self,
        tag: u16,
        cc: u32,
        handles: &[u32],
        auth: Option<&[u8]>,
        params: &[u8],
        n_handles: usize,
    ) -> Out<Response> {
        let mut body = W::default();
        for handle in handles {
            body.u32(*handle);
        }
        if let Some(auth) = auth {
            body.u32(u32::try_from(auth.len()).unwrap_or(u32::MAX))
                .raw(auth);
        }
        body.raw(params);
        let mut command = W::default();
        command
            .u16(tag)
            .u32(10 + u32::try_from(body.0.len()).unwrap_or(u32::MAX))
            .u32(cc)
            .raw(&body.0);
        let response = self.transport.transmit(&command.0)?;
        parse_response(&response, n_handles)
    }

    /// A command without sessions.
    fn plain(
        &mut self,
        cc: u32,
        handles: &[u32],
        params: &[u8],
        n_handles: usize,
    ) -> Out<Response> {
        self.command(ST_NO_SESSIONS, cc, handles, None, params, n_handles)
    }

    /// A command authorised with the empty password (owner hierarchy, the
    /// SRK, a PCR).
    fn with_password(
        &mut self,
        cc: u32,
        handles: &[u32],
        params: &[u8],
        n_handles: usize,
    ) -> Out<Response> {
        let mut auth = W::default();
        auth.u32(RS_PW).b(&[]).u8(0).b(&[]);
        self.command(ST_SESSIONS, cc, handles, Some(&auth.0), params, n_handles)
    }

    /// A command authorised with `session`: its first parameter encrypted
    /// when `attrs` has DECRYPT, the response HMAC verified, and the
    /// response's first parameter decrypted when `attrs` has ENCRYPT.
    #[allow(clippy::too_many_arguments)]
    fn with_session(
        &mut self,
        cc: u32,
        handles: &[u32],
        names: &[&[u8]],
        params: &[u8],
        session: &mut Session,
        attrs: u8,
        n_handles: usize,
    ) -> Out<Response> {
        let nonce_caller = random(32)?;
        let mut params = Zeroizing::new(params.to_vec());
        if attrs & ATTR_DECRYPT != 0 {
            let n = R::new(&params).u16()? as usize;
            let end = 2 + n;
            if params.len() < end {
                return Err("short parameter to encrypt".into());
            }
            let key_iv = kdfa(&session.key, "CFB", &nonce_caller, &session.nonce_tpm, 256);
            cfb(&key_iv, &mut params[2..end], true)?;
        }
        let code = cc.to_be_bytes();
        let mut cp = vec![&code[..]];
        cp.extend(names.iter().copied());
        cp.push(&params);
        let cp_hash = sha256(&cp);
        let mac = hmac_sign(
            &session.key,
            &[&cp_hash, &nonce_caller, &session.nonce_tpm, &[attrs]],
        );
        let mut auth = W::default();
        auth.u32(session.handle).b(&nonce_caller).u8(attrs).b(&mac);
        let mut parsed =
            self.command(ST_SESSIONS, cc, handles, Some(&auth.0), &params, n_handles)?;
        let [(nonce_tpm, r_attrs, r_mac)] = parsed.auths.as_slice() else {
            return Err("the response does not carry one session".into());
        };
        let rp_hash = sha256(&[&0u32.to_be_bytes(), &cc.to_be_bytes(), &parsed.params]);
        let signed = [&rp_hash[..], nonce_tpm, &nonce_caller, &[*r_attrs]].concat();
        hmac::verify(
            &hmac::Key::new(hmac::HMAC_SHA256, &session.key),
            &signed,
            r_mac,
        )
        .map_err(|_| "the response HMAC does not verify: refused")?;
        session.nonce_tpm.clone_from(nonce_tpm);
        if attrs & ATTR_ENCRYPT != 0 {
            let p = &mut parsed.params;
            let n = R::new(p).u16()? as usize;
            if p.len() < 2 + n {
                return Err("short encrypted parameter".into());
            }
            let key_iv = kdfa(&session.key, "CFB", &session.nonce_tpm, &nonce_caller, 256);
            cfb(&key_iv, &mut p[2..2 + n], false)?;
        }
        Ok(parsed)
    }

    /// The storage primary (ECC P-256, the TCG SRK template), its name
    /// checked against its public area.
    pub fn create_primary(&mut self) -> Out<Primary> {
        let mut public = W::default();
        public
            .u16(ALG_ECC)
            .u16(ALG_SHA256)
            .u32(0x0003_0472)
            .b(&[])
            .u16(ALG_AES)
            .u16(128)
            .u16(ALG_CFB)
            .u16(ALG_NULL)
            .u16(ECC_NIST_P256)
            .u16(ALG_NULL)
            .b(&[])
            .b(&[]);
        let mut params = W::default();
        params.u16(4).b(&[]).b(&[]); // inSensitive: no auth, no data
        params.b(&public.0).b(&[]).u32(0);
        let r = self.with_password(CC_CREATE_PRIMARY, &[RH_OWNER], &params.0, 1)?;
        let (area, name) = parse_create_primary(&r.params)?;
        let computed = [&ALG_SHA256.to_be_bytes()[..], &sha256(&[&area])].concat();
        if computed != name {
            self.flush(r.handles[0]);
            return Err("the primary's name does not match its public area".into());
        }
        let public = parse_ecc_public(&area);
        let public = match public {
            Ok(public) => public,
            Err(err) => {
                self.flush(r.handles[0]);
                return Err(err.into());
            }
        };
        Ok(Primary {
            handle: r.handles[0],
            public,
            name,
        })
    }

    /// A session salted to `primary` with an ephemeral ECDH exchange (KDFe),
    /// AES-128-CFB for parameter encryption.
    fn start_session(&mut self, primary: &Primary, kind: u8) -> Out<Session> {
        let mut point = vec![0x04];
        point.extend(pad32(&primary.public.x));
        point.extend(pad32(&primary.public.y));
        let ours = EphemeralPrivateKey::generate(&ECDH_P256, &SystemRandom::new())
            .map_err(|_| "ephemeral key")?;
        let ephemeral = ours.compute_public_key().map_err(|_| "ephemeral key")?;
        let ephemeral = ephemeral.as_ref();
        if ephemeral.len() != 65 {
            return Err("ephemeral point".into());
        }
        let (ex, ey) = (&ephemeral[1..33], &ephemeral[33..65]);
        let salt = agree_ephemeral(
            ours,
            UnparsedPublicKey::new(&ECDH_P256, &point),
            Failure::from("the storage key's point is not on P-256"),
            |z| Ok(kdfe(z, "SECRET", ex, &pad32(&primary.public.x), 256)),
        )?;
        let mut encrypted = W::default();
        encrypted.b(ex).b(ey);
        let nonce_caller = random(32)?;
        let mut params = W::default();
        params
            .b(&nonce_caller)
            .b(&encrypted.0)
            .u8(kind)
            .u16(ALG_AES)
            .u16(128)
            .u16(ALG_CFB)
            .u16(ALG_SHA256);
        let r = self.plain(
            CC_START_AUTH_SESSION,
            &[primary.handle, RH_NULL],
            &params.0,
            1,
        )?;
        let mut body = R::new(&r.params);
        let nonce_tpm = body.b()?.to_vec();
        body.done()?;
        let key = kdfa(&salt, "ATH", &nonce_tpm, &nonce_caller, 256);
        Ok(Session {
            handle: r.handles[0],
            key,
            nonce_tpm,
        })
    }

    pub fn pcr_read(&mut self, pcrs: &[u32]) -> Out<Vec<Vec<u8>>> {
        let r = self.plain(CC_PCR_READ, &[], &pcr_selection(pcrs), 0)?;
        Ok(parse_pcr_read(&r.params)?)
    }

    pub fn pcr_extend(&mut self, pcr: u32, digest: &[u8]) -> Out<()> {
        let mut params = W::default();
        params.u32(1).u16(ALG_SHA256).raw(digest);
        self.with_password(CC_PCR_EXTEND, &[pcr], &params.0, 0)?;
        Ok(())
    }

    pub fn flush(&mut self, handle: u32) {
        let _ = self.plain(CC_FLUSH_CONTEXT, &[], &handle.to_be_bytes(), 0);
    }

    /// Seal `secret` under `primary` to `policy`, the secret sent encrypted:
    /// (TPM2B_PRIVATE, TPM2B_PUBLIC) contents.
    pub fn seal(
        &mut self,
        primary: &Primary,
        secret: &[u8],
        policy: &[u8],
    ) -> Out<(Vec<u8>, Vec<u8>)> {
        let mut session = self.start_session(primary, SE_HMAC)?;
        let mut sensitive = W::default();
        sensitive.b(&[]).b(secret);
        let sensitive = Zeroizing::new(sensitive.0);
        let mut public = W::default();
        public
            .u16(ALG_KEYEDHASH)
            .u16(ALG_SHA256)
            .u32(0x0000_0412) // fixedTPM | fixedParent | noDA: policy only
            .b(policy)
            .u16(ALG_NULL)
            .b(&[]);
        let mut params = W::default();
        params.b(&sensitive).b(&public.0).b(&[]).u32(0);
        let params = Zeroizing::new(params.0);
        let result = self.with_session(
            CC_CREATE,
            &[primary.handle],
            &[&primary.name],
            &params,
            &mut session,
            ATTR_DECRYPT,
            0,
        );
        // Without continueSession the TPM ends the session with the
        // command; the flush covers a failure before it.
        self.flush(session.handle);
        Ok(parse_create(&result?.params)?)
    }

    /// Unseal `private`/`public` under `primary` through a PolicyPCR session
    /// over [`POLICY_PCRS`], the answer encrypted on the wire. PolicyPCR
    /// carries no digest: a PCR off the sealed policy shows as POLICY_FAIL.
    pub fn unseal(
        &mut self,
        primary: &Primary,
        private: &[u8],
        public: &[u8],
    ) -> Out<Zeroizing<Vec<u8>>> {
        let mut session = self.start_session(primary, SE_POLICY)?;
        let result = self.unseal_in(primary, &mut session, private, public);
        self.flush(session.handle);
        result
    }

    fn unseal_in(
        &mut self,
        primary: &Primary,
        session: &mut Session,
        private: &[u8],
        public: &[u8],
    ) -> Out<Zeroizing<Vec<u8>>> {
        let mut params = W::default();
        params.b(&[]).raw(&pcr_selection(&POLICY_PCRS));
        self.plain(CC_POLICY_PCR, &[session.handle], &params.0, 0)?;
        let mut params = W::default();
        params.b(private).b(public);
        let r = self.with_password(CC_LOAD, &[primary.handle], &params.0, 1)?;
        let item = r.handles[0];
        let result = (|| {
            let mut body = R::new(&r.params);
            let name = body.b()?.to_vec();
            body.done()?;
            let r =
                self.with_session(CC_UNSEAL, &[item], &[&name], &[], session, ATTR_ENCRYPT, 0)?;
            let params = Zeroizing::new(r.params);
            let mut body = R::new(&params);
            let out = Zeroizing::new(body.b()?.to_vec());
            body.done()?;
            Ok(out)
        })();
        self.flush(item);
        result
    }
}

fn pad32(v: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 32usize.saturating_sub(v.len())];
    out.extend_from_slice(v);
    out
}
