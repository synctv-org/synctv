#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg(verus_only)]
verus! { broadcast use vstd::seq_lib::group_seq_properties; }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(verus_only),
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
#[cfg_attr(verus_only, verus_verify)]
pub enum AuthFactorMethod {
    Password,
    WebAuthn,
    Totp,
    RecoveryCode,
    Email,
}

/// Independent factors. A recovery code is an alternative proof of the TOTP factor.
#[derive(Default)]
#[cfg_attr(verus_only, verus_verify)]
pub(super) struct Factors {
    pub(super) password: bool,
    pub(super) webauthn: bool,
    pub(super) totp: bool,
    pub(super) email: bool,
}

impl Factors {
    #[cfg_attr(verus_only, verus_spec(n => ensures n ==
        (self.password as int) + (self.webauthn as int) + (self.totp as int) + (self.email as int)))]
    pub(super) const fn count(&self) -> usize {
        self.password as usize + self.webauthn as usize + self.totp as usize + self.email as usize
    }

    #[cfg_attr(verus_only, verus_spec(list => ensures
        list@.contains(AuthFactorMethod::Password) == (self.password && !done.password),
        list@.contains(AuthFactorMethod::WebAuthn) == (self.webauthn && !done.webauthn),
        list@.contains(AuthFactorMethod::Totp) == (self.totp && !done.totp),
        list@.contains(AuthFactorMethod::RecoveryCode) == (self.totp && !done.totp && recovery),
        list@.contains(AuthFactorMethod::Email) == (self.email && !done.email),
        list@.no_duplicates(),
    ))]
    pub(super) fn available(&self, done: &Self, recovery: bool) -> Vec<AuthFactorMethod> {
        let mut methods = Vec::with_capacity(5);
        if self.password && !done.password {
            methods.push(AuthFactorMethod::Password);
        }
        if self.webauthn && !done.webauthn {
            methods.push(AuthFactorMethod::WebAuthn);
        }
        if self.totp && !done.totp {
            methods.push(AuthFactorMethod::Totp);
            if recovery {
                methods.push(AuthFactorMethod::RecoveryCode);
            }
        }
        if self.email && !done.email {
            methods.push(AuthFactorMethod::Email);
        }
        methods
    }
}

#[cfg_attr(verus_only, verus_spec(seen => ensures
    seen.password == methods@.contains(AuthFactorMethod::Password),
    seen.webauthn == methods@.contains(AuthFactorMethod::WebAuthn),
    seen.totp == (methods@.contains(AuthFactorMethod::Totp) || methods@.contains(AuthFactorMethod::RecoveryCode)),
    seen.email == methods@.contains(AuthFactorMethod::Email),
))]
pub(super) fn from_methods(methods: &[AuthFactorMethod]) -> Factors {
    let mut seen = Factors {
        password: false,
        webauthn: false,
        totp: false,
        email: false,
    };
    let mut i = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant i <= methods.len(),
            seen.password == methods@.take(i as int).contains(AuthFactorMethod::Password),
            seen.webauthn == methods@.take(i as int).contains(AuthFactorMethod::WebAuthn),
            seen.totp == (methods@.take(i as int).contains(AuthFactorMethod::Totp) ||
                methods@.take(i as int).contains(AuthFactorMethod::RecoveryCode)),
            seen.email == methods@.take(i as int).contains(AuthFactorMethod::Email),
        decreases methods.len() - i,
    ))]
    while i < methods.len() {
        #[cfg(verus_only)]
        proof! {
            assert(methods@.take(i + 1) =~= methods@.take(i as int).push(methods@[i as int]));
        }
        match methods[i] {
            AuthFactorMethod::Password => seen.password = true,
            AuthFactorMethod::WebAuthn => seen.webauthn = true,
            AuthFactorMethod::Totp | AuthFactorMethod::RecoveryCode => seen.totp = true,
            AuthFactorMethod::Email => seen.email = true,
        }
        i += 1;
    }
    #[cfg(verus_only)]
    proof! { assert(methods@.take(i as int) =~= methods@); }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_and_repeated_factors_cannot_complete_two_steps() {
        let done = from_methods(&[
            AuthFactorMethod::Totp,
            AuthFactorMethod::RecoveryCode,
            AuthFactorMethod::Totp,
        ]);
        assert_eq!(done.count(), 1);
        let enrolled = Factors {
            totp: true,
            email: true,
            ..Factors::default()
        };
        assert_eq!(
            enrolled.available(&done, true),
            vec![AuthFactorMethod::Email]
        );
    }
}
