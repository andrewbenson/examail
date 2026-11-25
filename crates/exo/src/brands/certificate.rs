//! Whether a brand's mark certificate (a Verified Mark or Common Mark Certificate) vouches for its
//! logo, checked as Gmail checks it: the chain up to one of the roots Gmail trusts for marks, every
//! certificate on it in date and issued for BIMI (its extended key usage), the sending domain or
//! its organizational domain among the certificate's names, and the logo's hash among those of the
//! logo the certificate carries (its logotype extension, RFC 3709). And whether its CA has revoked
//! it since, by the CA's revocation list.

use std::{sync::LazyLock, time::Duration};

use jiff::Timestamp;
use rustls_pki_types::{CertificateDer, ServerName, TrustAnchor, UnixTime, pem::PemObject};
use sha2::{Digest, Sha256};
use x509_parser::{
    asn1_rs::{Any, Class, FromDer as _, Oid, Tag},
    extensions::{DistributionPointName, GeneralName, ParsedExtension},
    oid_registry::OID_NIST_HASH_SHA256,
};

/// `id-kp-BrandIndicatorforMessageIdentification` (1.3.6.1.5.5.7.3.31), the extended key usage of
/// mark certificates and of the CAs that issue them.
const BRAND_INDICATOR: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x1f];
/// `id-pe-logotype` (1.3.6.1.5.5.7.1.12), the extension that carries the mark.
const LOGOTYPE_OID: Oid<'static> = x509_parser::der_parser::oid!(1.3.6.1.5.5.7.1.12);

/// The roots Gmail trusts for mark certificates: the issuers the BIMI Group lists.
static ROOTS: LazyLock<Vec<TrustAnchor<'static>>> = LazyLock::new(|| {
    macro_rules! root {
        ($name:literal) => {
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/mark-roots/",
                $name
            ))
            .as_slice()
        };
    }
    [
        root!("DigiCertVerifiedMarkRootCA.pem"),
        root!("GlobalSignVerifiedMarkRootR42.pem"),
        root!("SSLcomVMCECCRootCA2024.pem"),
        root!("SSLcomVMCRSARootCA2024.pem"),
    ]
    .into_iter()
    .filter_map(|pem| {
        let der = CertificateDer::from_pem_slice(pem).ok()?;
        Some(webpki::anchor_from_trusted_cert(&der).ok()?.to_owned())
    })
    .collect()
});

/// Why a certificate doesn't vouch for a logo.
#[derive(Debug, thiserror::Error)]
pub(super) enum Invalid {
    #[error("the file holds no certificate")]
    Empty,
    #[error("the certificate doesn't parse")]
    Malformed,
    #[error("the certificate doesn't validate: {0}")]
    Chain(#[from] webpki::Error),
    #[error("the certificate doesn't name {0}")]
    Domain(String),
    #[error("the logo isn't the certificate's")]
    Logo,
}

/// Until when the mark certificate `pem` (the certificate, then its issuers) vouches for `svg`, an
/// uncompressed SVG, as the logo of the sending domain `domain`, at `now`.
pub(super) fn validate(
    pem: &[u8],
    svg: &[u8],
    domain: &str,
    now: Timestamp,
) -> Result<Timestamp, Invalid> {
    validate_against(&ROOTS, pem, svg, domain, now)
}

fn validate_against(
    roots: &[TrustAnchor<'_>],
    pem: &[u8],
    svg: &[u8],
    domain: &str,
    now: Timestamp,
) -> Result<Timestamp, Invalid> {
    let chain = chain(pem)?;
    let certificate = verify(roots, &chain, now, None)?;
    if !names(&certificate, domain) {
        return Err(Invalid::Domain(domain.to_owned()));
    }
    let (_, x509) =
        x509_parser::parse_x509_certificate(&chain[0]).map_err(|_| Invalid::Malformed)?;
    let not_after = Timestamp::from_second(x509.validity().not_after.timestamp())
        .map_err(|_| Invalid::Malformed)?;
    let logotype = x509
        .get_extension_unique(&LOGOTYPE_OID)
        .map_err(|_| Invalid::Malformed)?
        .ok_or(Invalid::Malformed)?;
    let expected = Sha256::digest(svg);
    let vouched = svg_hashes(logotype.value)
        .into_iter()
        .any(|(algorithm, hash)| algorithm == OID_NIST_HASH_SHA256 && hash == expected.as_slice());
    if !vouched {
        return Err(Invalid::Logo);
    }
    Ok(not_after)
}

/// Whether `crl`, a revocation list, says the CA revoked the certificate in `pem` (as
/// [`validate`] takes it), at `now`. `None` when the list can't tell: it isn't that CA's, it's
/// past its next update, or it doesn't verify.
pub(super) fn revoked(pem: &[u8], crl: &[u8], now: Timestamp) -> Option<bool> {
    revoked_against(&ROOTS, pem, crl, now)
}

fn revoked_against(
    roots: &[TrustAnchor<'_>],
    pem: &[u8],
    crl: &[u8],
    now: Timestamp,
) -> Option<bool> {
    let chain = chain(pem).ok()?;
    let crl =
        webpki::CertRevocationList::from(webpki::BorrowedCertRevocationList::from_der(crl).ok()?);
    let crls = [&crl];
    let revocation = webpki::RevocationOptionsBuilder::new(&crls)
        .ok()?
        .with_depth(webpki::RevocationCheckDepth::EndEntity)
        .with_status_policy(webpki::UnknownStatusPolicy::Deny)
        .with_expiration_policy(webpki::ExpirationPolicy::Enforce)
        .build();
    match verify(roots, &chain, now, Some(revocation)) {
        Ok(_) => Some(false),
        Err(Invalid::Chain(webpki::Error::CertRevoked)) => Some(true),
        Err(invalid) => {
            log::info!("a revocation list can't tell: {invalid}");
            None
        }
    }
}

/// Where the certificate in `pem` says its CA's revocation list is: its CRL distribution points'
/// URLs, in order.
pub(super) fn revocation_lists(pem: &[u8]) -> Vec<String> {
    let Ok(chain) = chain(pem) else {
        return Vec::new();
    };
    let Some(der) = chain.first() else {
        return Vec::new();
    };
    let Ok((_, x509)) = x509_parser::parse_x509_certificate(der) else {
        return Vec::new();
    };
    x509.extensions()
        .iter()
        .filter_map(|ext| match ext.parsed_extension() {
            ParsedExtension::CRLDistributionPoints(points) => Some(&points.points),
            _ => None,
        })
        .flatten()
        .filter_map(|point| match point.distribution_point.as_ref()? {
            DistributionPointName::FullName(names) => Some(names),
            DistributionPointName::NameRelativeToCRLIssuer(_) => None,
        })
        .flatten()
        .filter_map(|name| match name {
            GeneralName::URI(uri) => Some((*uri).to_owned()),
            _ => None,
        })
        .collect()
}

/// The certificates in `pem`: the end entity's, then its issuers'.
fn chain(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, Invalid> {
    CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| Invalid::Malformed)
}

/// The end entity of `chain`, once its chain up to `roots` checks at `now` for BIMI, and with
/// `revocation`, its CA's revocation list.
fn verify<'a>(
    roots: &[TrustAnchor<'_>],
    chain: &'a [CertificateDer<'a>],
    now: Timestamp,
    revocation: Option<webpki::RevocationOptions<'_>>,
) -> Result<webpki::EndEntityCert<'a>, Invalid> {
    let (der, issuers) = chain.split_first().ok_or(Invalid::Empty)?;
    let certificate = webpki::EndEntityCert::try_from(der)?;
    let time = UnixTime::since_unix_epoch(Duration::from_secs(
        u64::try_from(now.as_second()).unwrap_or_default(),
    ));
    certificate.verify_for_usage(
        webpki::ALL_VERIFICATION_ALGS,
        roots,
        issuers,
        time,
        webpki::KeyUsage::required(BRAND_INDICATOR),
        revocation,
        None,
    )?;
    Ok(certificate)
}

/// Whether `certificate` names `domain` or its organizational domain, as itself or as its default
/// BIMI selector (`default._bimi.<domain>`).
fn names(certificate: &webpki::EndEntityCert<'_>, domain: &str) -> bool {
    let organization = psl::domain_str(domain).unwrap_or(domain);
    [domain, organization]
        .into_iter()
        .flat_map(|domain| [domain.to_owned(), format!("default._bimi.{domain}")])
        .filter_map(|name| ServerName::try_from(name).ok())
        .any(|name| certificate.verify_is_valid_for_subject_name(&name).is_ok())
}

/// The hashes of the SVG images of a logotype extension's subject logo, each with its algorithm.
fn svg_hashes(logotype: &[u8]) -> Vec<(Oid<'_>, &[u8])> {
    // LogotypeExtn ::= SEQUENCE { …, subjectLogo [2] EXPLICIT LogotypeInfo, … }, whose
    // `direct [0] IMPLICIT LogotypeData` starts with `image SEQUENCE OF LogotypeImage`.
    let images = first_tag(logotype, Class::Universal, Tag::Sequence)
        .and_then(|extn| first_tag(extn, Class::ContextSpecific, Tag(2)))
        .and_then(|subject| first_tag(subject, Class::ContextSpecific, Tag(0)))
        .and_then(|direct| first_tag(direct, Class::Universal, Tag::Sequence))
        .unwrap_or_default();
    // LogotypeImage ::= SEQUENCE { imageDetails LogotypeDetails, … }, and
    // LogotypeDetails ::= SEQUENCE { mediaType IA5String, logotypeHash SEQUENCE OF HashAlgAndValue,
    // logotypeURI … }.
    der_items(images)
        .filter_map(|image| first_tag(image.data, Class::Universal, Tag::Sequence))
        .filter_map(|details| {
            let mut fields = der_items(details);
            let media = fields.next()?;
            (media.tag() == Tag::Ia5String && media.data == b"image/svg+xml")
                .then(|| fields.next())?
        })
        .flat_map(|hashes| der_items(hashes.data))
        .filter_map(|hash| {
            // HashAlgAndValue ::= SEQUENCE { hashAlg AlgorithmIdentifier, hashValue OCTET STRING }
            let mut parts = der_items(hash.data);
            let (_, algorithm) = Oid::from_der(parts.next()?.data).ok()?;
            let value = parts.next()?;
            (value.tag() == Tag::OctetString).then_some((algorithm, value.data))
        })
        .collect()
}

fn der_items(mut input: &[u8]) -> impl Iterator<Item = Any<'_>> {
    std::iter::from_fn(move || {
        let (rest, item) = Any::from_der(input).ok()?;
        input = rest;
        Some(item)
    })
}

fn first_tag(input: &[u8], class: Class, tag: Tag) -> Option<&[u8]> {
    der_items(input)
        .find(|item| item.class() == class && item.tag() == tag)
        .map(|item| item.data)
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use rcgen::{
        BasicConstraints, CertificateParams, CertificateRevocationListParams, CertifiedIssuer,
        CrlDistributionPoint, CrlIssuingDistributionPoint, CustomExtension, DistinguishedName,
        DnType, ExtendedKeyUsagePurpose, IsCa, KeyIdMethod, KeyPair, RevokedCertParams,
        SerialNumber, date_time_ymd,
    };

    use super::*;

    const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" version="1.2" baseProfile="tiny-ps" viewBox="0 0 96 96"><title>Brand</title><rect width="96" height="96" fill="#0a7"/></svg>"##;
    const SHA1: &[u8] = &[0x2b, 0x0e, 0x03, 0x02, 0x1a];
    const SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
    const BRAND_INDICATOR: [u64; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 31];
    /// Where the test CA's certificates say its revocation list is, and its mirror.
    const LISTS: [&str; 2] = ["http://crl.example/ca.crl", "http://crl2.example/ca.crl"];
    /// The serial number of the certificates the CA issues here.
    const SERIAL: &[u8] = &[0x42];

    const OCTET_STRING: u8 = 0x04;
    const OID: u8 = 0x06;
    const SEQUENCE: u8 = 0x30;
    const CONTEXT_0: u8 = 0xa0;
    const CONTEXT_2: u8 = 0xa2;

    fn now() -> Timestamp {
        "2026-06-01T00:00:00Z".parse().expect("a time")
    }

    /// A DER element tagged `tag` holding `parts`.
    fn der(tag: u8, parts: &[&[u8]]) -> Vec<u8> {
        let contents = parts.concat();
        let length = contents.len();
        let mut element = vec![tag];
        match u8::try_from(length) {
            Ok(short @ 0..0x80) => element.push(short),
            Ok(byte) => element.extend([0x81, byte]),
            Err(_) => element.extend([0x82, (length >> 8) as u8, length as u8]),
        }
        element.extend(contents);
        element
    }

    /// A logotype extension carrying `svg` with `hash` tagged by `algorithm`.
    fn logotype_with_hash(svg: &[u8], algorithm: &[u8], hash: &[u8]) -> Vec<u8> {
        let uri = format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(svg)
        );
        let details = der(
            SEQUENCE,
            &[
                &der(0x16, &[b"image/svg+xml"]),
                &der(
                    SEQUENCE,
                    &[&der(
                        SEQUENCE,
                        &[
                            &der(SEQUENCE, &[&der(OID, &[algorithm])]),
                            &der(OCTET_STRING, &[hash]),
                        ],
                    )],
                ),
                &der(SEQUENCE, &[&der(0x16, &[uri.as_bytes()])]),
            ],
        );
        let images = der(SEQUENCE, &[&der(SEQUENCE, &[&details])]);
        der(SEQUENCE, &[&der(CONTEXT_2, &[&der(CONTEXT_0, &[&images])])])
    }

    /// A logotype extension carrying `svg` with its SHA-256 hash, as BIMI mark certificates do.
    fn logotype(svg: &[u8]) -> Vec<u8> {
        let hash = Sha256::digest(svg);
        logotype_with_hash(svg, SHA256, hash.as_slice())
    }

    /// A mark root, the CA it issued, and what the CA issues.
    struct Marks {
        root: CertifiedIssuer<'static, KeyPair>,
        ca: CertifiedIssuer<'static, KeyPair>,
    }

    impl Marks {
        fn new() -> Self {
            Self::with_ca_usages(vec![ExtendedKeyUsagePurpose::Other(
                BRAND_INDICATOR.to_vec(),
            )])
        }

        fn with_ca_usages(usages: Vec<ExtendedKeyUsagePurpose>) -> Self {
            let authority = |name: &str| {
                let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
                params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
                params.distinguished_name = DistinguishedName::new();
                params.distinguished_name.push(DnType::CommonName, name);
                params
            };
            let root = CertifiedIssuer::self_signed(
                authority("Mark Root"),
                KeyPair::generate().expect("a key"),
            )
            .expect("the root");
            let mut ca = authority("Mark CA");
            ca.extended_key_usages = usages;
            let ca = CertifiedIssuer::signed_by(ca, KeyPair::generate().expect("a key"), &root)
                .expect("the CA");
            Self { root, ca }
        }

        fn anchors(&self) -> Vec<TrustAnchor<'static>> {
            vec![
                webpki::anchor_from_trusted_cert(self.root.der())
                    .expect("an anchor")
                    .to_owned(),
            ]
        }

        /// The PEM a brand serves: its certificate for `names`, carrying `logotype`, then the CA.
        fn pem(
            &self,
            names: &[&str],
            logotype: Vec<u8>,
            usages: Vec<ExtendedKeyUsagePurpose>,
        ) -> String {
            let mut params = CertificateParams::new(
                names
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<Vec<_>>(),
            )
            .expect("params");
            params.not_before = date_time_ymd(2026, 1, 1);
            params.not_after = date_time_ymd(2027, 1, 1);
            params.serial_number = Some(SerialNumber::from_slice(SERIAL));
            params.crl_distribution_points = vec![CrlDistributionPoint {
                uris: LISTS.map(str::to_owned).to_vec(),
            }];
            params.extended_key_usages = usages;
            params.custom_extensions = vec![CustomExtension::from_oid_content(
                &[1, 3, 6, 1, 5, 5, 7, 1, 12],
                logotype,
            )];
            let leaf = params
                .signed_by(&KeyPair::generate().expect("a key"), &self.ca)
                .expect("the certificate");
            format!("{}{}", leaf.pem(), self.ca.pem())
        }

        fn brand(&self, names: &[&str]) -> String {
            self.pem(
                names,
                logotype(SVG),
                vec![ExtendedKeyUsagePurpose::Other(BRAND_INDICATOR.to_vec())],
            )
        }

        /// A revocation list from `issuer`, listing `serials`, current until `next_update`.
        fn crl(
            issuer: &CertifiedIssuer<'static, KeyPair>,
            serials: &[&[u8]],
            next_update: (i32, u8, u8),
        ) -> Vec<u8> {
            let params = CertificateRevocationListParams {
                this_update: date_time_ymd(2026, 5, 1),
                next_update: date_time_ymd(next_update.0, next_update.1, next_update.2),
                crl_number: SerialNumber::from(1u64),
                issuing_distribution_point: Some(CrlIssuingDistributionPoint {
                    distribution_point: CrlDistributionPoint {
                        uris: LISTS.map(str::to_owned).to_vec(),
                    },
                    scope: None,
                }),
                revoked_certs: serials
                    .iter()
                    .map(|serial| RevokedCertParams {
                        serial_number: SerialNumber::from_slice(serial),
                        revocation_time: date_time_ymd(2026, 4, 1),
                        reason_code: None,
                        invalidity_date: None,
                    })
                    .collect(),
                key_identifier_method: KeyIdMethod::Sha256,
            };
            params
                .signed_by(issuer)
                .expect("the revocation list")
                .der()
                .to_vec()
        }

        fn revoked(&self, pem: &str, crl: &[u8]) -> Option<bool> {
            revoked_against(&self.anchors(), pem.as_bytes(), crl, now())
        }

        fn validate(
            &self,
            pem: &str,
            svg: &[u8],
            domain: &str,
            now: Timestamp,
        ) -> Result<Timestamp, Invalid> {
            validate_against(&self.anchors(), pem.as_bytes(), svg, domain, now)
        }
    }

    #[test]
    fn a_mark_certificate_vouches_for_its_logo_until_it_expires() {
        let marks = Marks::new();
        let pem = marks.brand(&["brand.example"]);
        let until = marks.validate(&pem, SVG, "brand.example", now());
        assert_eq!(
            until.ok(),
            Some("2027-01-01T00:00:00Z".parse().expect("a time"))
        );
        // A subdomain's mail takes its organizational domain's certificate.
        assert!(
            marks
                .validate(&pem, SVG, "news.brand.example", now())
                .is_ok()
        );
        // Past its end, it vouches for nothing.
        let later = "2027-02-01T00:00:00Z".parse().expect("a time");
        assert!(matches!(
            marks.validate(&pem, SVG, "brand.example", later),
            Err(Invalid::Chain(webpki::Error::CertExpired { .. }))
        ));
    }

    #[test]
    fn sha1_logotype_hashes_are_rejected() {
        /// SHA-1 digest of [`SVG`], confirming even a matching SHA-1 hash is rejected.
        const SVG_SHA1: [u8; 20] = [
            0x27, 0xe7, 0x6c, 0x92, 0xfb, 0x68, 0xbf, 0xa2, 0xd4, 0xb4, 0x3e, 0x44, 0x73, 0xa1,
            0xa6, 0xe6, 0xc7, 0x2a, 0x36, 0xef,
        ];
        let marks = Marks::new();
        let pem = marks.pem(
            &["brand.example"],
            logotype_with_hash(SVG, SHA1, &SVG_SHA1),
            vec![ExtendedKeyUsagePurpose::Other(BRAND_INDICATOR.to_vec())],
        );
        assert!(matches!(
            marks.validate(&pem, SVG, "brand.example", now()),
            Err(Invalid::Logo)
        ));
    }

    #[test]
    fn a_certificate_names_its_domain_or_its_default_selector() {
        let marks = Marks::new();
        let selector = marks.brand(&["default._bimi.brand.example"]);
        assert!(
            marks
                .validate(&selector, SVG, "brand.example", now())
                .is_ok()
        );
        let other = marks.brand(&["other.example", "selector._bimi.brand.example"]);
        assert!(matches!(
            marks.validate(&other, SVG, "brand.example", now()),
            Err(Invalid::Domain(_))
        ));
    }

    #[test]
    fn a_logo_the_certificate_doesnt_carry_isnt_vouched_for() {
        let marks = Marks::new();
        let pem = marks.brand(&["brand.example"]);
        let other = String::from_utf8_lossy(SVG).replace("#0a7", "#d00");
        assert!(matches!(
            marks.validate(&pem, other.as_bytes(), "brand.example", now()),
            Err(Invalid::Logo)
        ));
    }

    #[test]
    fn only_bimi_certificates_from_the_mark_roots_count() {
        let marks = Marks::new();
        // Not issued by a root Gmail trusts for marks.
        let pem = marks.brand(&["brand.example"]);
        assert!(matches!(
            validate(pem.as_bytes(), SVG, "brand.example", now()),
            Err(Invalid::Chain(webpki::Error::UnknownIssuer))
        ));
        // A certificate for TLS isn't one for BIMI.
        let tls = marks.pem(
            &["brand.example"],
            logotype(SVG),
            vec![ExtendedKeyUsagePurpose::ServerAuth],
        );
        assert!(matches!(
            marks.validate(&tls, SVG, "brand.example", now()),
            Err(Invalid::Chain(webpki::Error::RequiredEkuNotFoundContext(_)))
        ));
        // Nor is one from a CA that doesn't issue them.
        let tls_ca = Marks::with_ca_usages(vec![ExtendedKeyUsagePurpose::ServerAuth]);
        let pem = tls_ca.brand(&["brand.example"]);
        assert!(matches!(
            tls_ca.validate(&pem, SVG, "brand.example", now()),
            Err(Invalid::Chain(webpki::Error::RequiredEkuNotFoundContext(_)))
        ));
        assert!(matches!(
            marks.validate("", SVG, "brand.example", now()),
            Err(Invalid::Empty)
        ));
    }

    #[test]
    fn a_cas_revocation_list_says_whether_it_revoked_a_certificate() {
        let marks = Marks::new();
        let pem = marks.brand(&["brand.example"]);
        assert_eq!(revocation_lists(pem.as_bytes()), LISTS);
        let listed = Marks::crl(&marks.ca, &[SERIAL], (2026, 7, 1));
        assert_eq!(marks.revoked(&pem, &listed), Some(true));
        let others = Marks::crl(&marks.ca, &[&[0x07]], (2026, 7, 1));
        assert_eq!(marks.revoked(&pem, &others), Some(false));
        // A list past its next update, the root's (not the CA's), or no list at all can't tell.
        let stale = Marks::crl(&marks.ca, &[SERIAL], (2026, 5, 15));
        assert_eq!(marks.revoked(&pem, &stale), None);
        let roots = Marks::crl(&marks.root, &[SERIAL], (2026, 7, 1));
        assert_eq!(marks.revoked(&pem, &roots), None);
        assert_eq!(marks.revoked(&pem, b"not a list"), None);
    }

    #[test]
    fn the_mark_roots_all_load() {
        assert_eq!(ROOTS.len(), 4);
    }

    #[test]
    fn certificate_times_read_both_forms() {
        // Dates through 2049 encode as UTCTime; 2050 and beyond encode as GeneralizedTime.
        let marks = Marks::new();
        let mut params = CertificateParams::new(vec!["brand.example".to_owned()]).expect("params");
        params.not_before = date_time_ymd(2026, 1, 1);
        params.not_after = date_time_ymd(2051, 1, 1);
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::Other(BRAND_INDICATOR.to_vec())];
        params.custom_extensions = vec![CustomExtension::from_oid_content(
            &[1, 3, 6, 1, 5, 5, 7, 1, 12],
            logotype(SVG),
        )];
        let leaf = params
            .signed_by(&KeyPair::generate().expect("a key"), &marks.ca)
            .expect("the certificate");
        let pem = format!("{}{}", leaf.pem(), marks.ca.pem());
        assert_eq!(
            marks.validate(&pem, SVG, "brand.example", now()).ok(),
            "2051-01-01T00:00:00Z".parse().ok()
        );
    }
}
