use std::time::Duration;

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use failure::Error;
use gcrypt::mpi::integer::{Format, Integer};
use nom::{rest, be_u16, be_u32, be_u64, be_u8};
use nom::{ErrorKind, IResult};

use types::*;

#[cfg_attr(rustfmt, rustfmt_skip)]
named!(
    v3_sig<SignaturePacket>,
    do_parse!(
        tag!(b"\x03") >>
        tag!(b"\x05") >>
        signature_type: be_u8 >>
        creation_time: be_u32 >>
        signer: be_u64 >>
        pubkey_algo: be_u8 >>
        hash_algo: be_u8 >>
        take!(2) >>
        signature: call!(rest) >>
        (SignaturePacket {
            sig_type: SignatureType::from(signature_type),
            timestamp: Some(Duration::from_secs(u64::from(creation_time))),
            signer: Some(signer),
            pubkey_algo: PublicKeyAlgorithm::from(pubkey_algo),
            hash_algo: HashAlgorithm::from(hash_algo),
            hashed_subpackets: Vec::new(),
            unhashed_subpackets: Vec::new(),
            signature_contents: Vec::from(signature),
        })
    )
);

fn subpacket_length(inp: &[u8]) -> IResult<&[u8], u32> {
    let (remaining, first_octet) = match be_u8(inp) {
        IResult::Done(remaining, first_octet) => (remaining, first_octet),
        IResult::Error(e) => return IResult::Error(e),
        IResult::Incomplete(i) => return IResult::Incomplete(i),
    };

    if first_octet < 192 {
        return IResult::Done(remaining, u32::from(first_octet));
    } else if first_octet < 255 {
        let (remaining, second_octet) = match be_u8(remaining) {
            IResult::Done(remaining, second_octet) => (remaining, second_octet),
            IResult::Error(e) => return IResult::Error(e),
            IResult::Incomplete(i) => return IResult::Incomplete(i),
        };

        let length = ((first_octet as u16 - 192) << 8) + second_octet as u16 + 192;

        return IResult::Done(remaining, u32::from(length));
    } else {
        return be_u32(remaining);
    }
}

fn parse_subpacket(inp: &[u8]) -> IResult<&[u8], Subpacket> {
    let (remaining, length) = match subpacket_length(inp) {
        IResult::Done(remaining, length) => (remaining, length),
        IResult::Error(e) => return IResult::Error(e),
        IResult::Incomplete(i) => return IResult::Incomplete(i),
    };

    let (remaining, subpacket_type) = match be_u8(remaining) {
        IResult::Done(remaining, subpacket_type) => (remaining, subpacket_type),
        IResult::Error(e) => return IResult::Error(e),
        IResult::Incomplete(i) => return IResult::Incomplete(i),
    };

    let (remaining, mut packet_contents) = match take!(remaining, length - 1) {
        IResult::Done(remaining, contents) => (remaining, contents),
        IResult::Error(e) => return IResult::Error(e),
        IResult::Incomplete(i) => return IResult::Incomplete(i),
    };

    match subpacket_type {
        0 | 1 | 8 | 13 | 14 | 15 | 17 | 18 | 19 => IResult::Error(ErrorKind::Custom(2)),
        2 => {
            let time_secs = match packet_contents.read_u32::<BigEndian>() {
                Ok(val) => val,
                Err(_) => return IResult::Error(ErrorKind::Custom(3)),
            };
            let subpacket =
                Subpacket::SignatureCreationTime(Duration::from_secs(u64::from(time_secs)));
            IResult::Done(remaining, subpacket)
        }
        3 => {
            let time_secs = match packet_contents.read_u32::<BigEndian>() {
                Ok(val) => val,
                Err(_) => return IResult::Error(ErrorKind::Custom(3)),
            };
            let subpacket =
                Subpacket::SignatureExpirationTime(Duration::from_secs(u64::from(time_secs)));
            IResult::Done(remaining, subpacket)
        }
        16 => {
            let issuer = match packet_contents.read_u64::<BigEndian>() {
                Ok(val) => val,
                Err(_) => return IResult::Error(ErrorKind::Custom(3)),
            };
            let subpacket = Subpacket::Issuer(issuer);
            IResult::Done(remaining, subpacket)
        }
        t => IResult::Done(remaining, Subpacket::Unknown(t, length)),
    }
}

named!(subpackets<Vec<Subpacket>>, many0!(parse_subpacket));

fn find_timestamp(subpackets: &[Subpacket]) -> Option<Duration> {
    for subpacket in subpackets {
        if let &Subpacket::SignatureCreationTime(out) = subpacket {
            return Some(out);
        }
    }

    None
}

fn find_signer(subpackets: &[Subpacket]) -> Option<u64> {
    for subpacket in subpackets {
        if let &Subpacket::Issuer(out) = subpacket {
            return Some(out);
        }
    }

    None
}

#[cfg_attr(rustfmt, rustfmt_skip)]
named!(
    v4_sig<SignaturePacket>,
    do_parse!(
        tag!(b"\x04") >>
        signature_type: be_u8 >>
        pubkey_algo: be_u8 >>
        hash_algo: be_u8 >>
        hashed_subs: length_value!(be_u16, subpackets) >>
        unhashed_subs: length_value!(be_u16, subpackets) >>
        take!(2) >>
        signature: call!(rest) >>
        (SignaturePacket {
            sig_type: SignatureType::from(signature_type),
            timestamp: find_timestamp(&hashed_subs).or(find_timestamp(&unhashed_subs)),
            signer: find_signer(&hashed_subs).or(find_signer(&unhashed_subs)),
            pubkey_algo: PublicKeyAlgorithm::from(pubkey_algo),
            hash_algo: HashAlgorithm::from(hash_algo),
            hashed_subpackets: hashed_subs,
            unhashed_subpackets: unhashed_subs,
            signature_contents: Vec::from(signature),
        })
    )
);

named!(signature<SignaturePacket>, alt!(v3_sig | v4_sig));

#[derive(Clone, Debug)]
pub struct SignaturePacket {
    sig_type: SignatureType,
    timestamp: Option<Duration>,
    signer: Option<u64>,
    pubkey_algo: PublicKeyAlgorithm,
    hash_algo: HashAlgorithm,
    hashed_subpackets: Vec<Subpacket>,
    unhashed_subpackets: Vec<Subpacket>,
    signature_contents: Vec<u8>,
}

impl SignaturePacket {
    pub fn from_bytes(bytes: &[u8]) -> Result<SignaturePacket, Error> {
        match signature(bytes) {
            IResult::Done(_, sig) => Ok(sig),
            IResult::Error(ErrorKind::Custom(e)) => {
                let e = NomError::from(e);

                bail!(SignatureError::InvalidFormat {
                    reason: format!("{:?}", e),
                })
            }
            IResult::Error(e) => bail!(SignatureError::InvalidFormat {
                reason: format!("{}", e),
            }),
            IResult::Incomplete(i) => bail!(SignatureError::InvalidFormat {
                reason: format!("{:?}", i),
            }),
        }
    }

    pub fn contents(&self) -> Result<Signature, Error> {
        match self.pubkey_algo {
            PublicKeyAlgorithm::Rsa
            | PublicKeyAlgorithm::RsaEncryptOnly
            | PublicKeyAlgorithm::RsaSignOnly => {
                let mpi = Integer::from_bytes(Format::Pgp, &self.signature_contents)?;
                Ok(Signature::Rsa(mpi))
            }
            PublicKeyAlgorithm::Dsa => {
                let mpi_r = Integer::from_bytes(Format::Pgp, &self.signature_contents)?;
                let s_pos = mpi_r.len_encoded(Format::Pgp)?;
                let mpi_s = Integer::from_bytes(Format::Pgp, &self.signature_contents[s_pos..])?;

                Ok(Signature::Dsa(mpi_r, mpi_s))
            }
            _ => Ok(Signature::Unknown(self.signature_contents.clone())),
        }
    }

    pub fn set_contents(&mut self, sig: Signature) -> Result<(), Error> {
        match sig {
            Signature::Rsa(mpi) => {
                self.signature_contents = Vec::from(mpi.to_bytes(Format::Pgp)?.as_bytes())
            }
            Signature::Dsa(r, s) => {
                let mut r_vec = Vec::from(r.to_bytes(Format::Pgp)?.as_bytes());
                let mut s_vec = Vec::from(s.to_bytes(Format::Pgp)?.as_bytes());
                r_vec.append(&mut s_vec);

                self.signature_contents = r_vec;
            }
            Signature::Unknown(payload) => self.signature_contents = payload.clone(),
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SignatureType {
    BinaryDocument = 0x00,
    TextDocument = 0x01,
    Standalone = 0x02,
    GenericCertification = 0x10,
    PersonaCertification = 0x11,
    CasualCertification = 0x12,
    PositiveCertification = 0x13,
    SubkeyBinding = 0x18,
    PrimaryKeyBinding = 0x19,
    DirectKey = 0x1F,
    KeyRevocation = 0x20,
    SubkeyRevocation = 0x28,
    CertificationRevocation = 0x30,
    Timestamp = 0x40,
    ThirdPartyConfirmation = 0x50,
    Unknown = 255,
}

impl From<u8> for SignatureType {
    fn from(val: u8) -> SignatureType {
        match val {
            0x00 => SignatureType::BinaryDocument,
            0x01 => SignatureType::TextDocument,
            0x02 => SignatureType::Standalone,
            0x10 => SignatureType::GenericCertification,
            0x11 => SignatureType::PersonaCertification,
            0x12 => SignatureType::CasualCertification,
            0x13 => SignatureType::PositiveCertification,
            0x18 => SignatureType::SubkeyBinding,
            0x19 => SignatureType::PrimaryKeyBinding,
            0x1F => SignatureType::DirectKey,
            0x20 => SignatureType::KeyRevocation,
            0x28 => SignatureType::SubkeyRevocation,
            0x30 => SignatureType::CertificationRevocation,
            0x40 => SignatureType::Timestamp,
            0x50 => SignatureType::ThirdPartyConfirmation,
            _ => SignatureType::Unknown,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Subpacket {
    SignatureCreationTime(Duration),
    SignatureExpirationTime(Duration),
    ExportableCertification,
    TrustSignature,
    RegularExpression,
    Revocable,
    KeyExpirationTime(Duration),
    PreferredSymmetricAlgorithms,
    RevocationKey,
    Issuer(u64),
    NotationData,
    PreferredHashAlgorithms,
    PreferredCompressionAlgorithms,
    KeyServerPreferences,
    PreferredKeyServer,
    PrimaryUserId,
    PolicyUri,
    KeyFlags,
    SignerUserId,
    RevocationReason,
    Features,
    SignatureTarget,
    EmbeddedSignature,
    Unknown(u8, u32),
}

impl Subpacket {
    fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let mut out: Vec<u8> = Vec::new();

        match *self {
            Subpacket::SignatureCreationTime(time) => {
                // Subpacket type
                out.push(2);
                out.write_u32::<BigEndian>(time.as_secs() as u32)?;
            }
            Subpacket::Issuer(issuer) => {
                // Subpacket type
                out.push(16);
                out.write_u64::<BigEndian>(issuer)?;
            }
            _ => {}
        }

        Ok(out)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signature {
    Rsa(Integer),
    Dsa(Integer, Integer),
    Unknown(Vec<u8>),
}

#[derive(Debug, Fail)]
pub enum SignatureError {
    #[fail(display = "Invalid signature format: {}", reason)] InvalidFormat { reason: String },
}
