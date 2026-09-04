use std::fmt::{Display, Formatter, Result as FmtResult};

use serde::{Deserialize, Serialize};

/// The organization which is responsible for the competition
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum OrganizationDTO {
    /// Landesfachverbände
    LFV(LandesturnverbandDTO),
    /// Deutscher Turner-Bund
    DTB,
    /// Internationaler Rhönradturnverband
    IRV,
}

impl Display for OrganizationDTO {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let name = match self {
            Self::LFV(lfv) => return lfv.fmt(f),
            Self::DTB => "Deutscher Turner-Bund",
            Self::IRV => "Internationaler Rhönradturnverband",
        };
        f.write_str(name)
    }
}

/// All German LFV which are member of the DTB
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum LandesturnverbandDTO {
    LfvBadischerTurnerBund,
    LfvBayerischerTurnverband,
    LfvBerlinerTurnUndFreizeitsportBund,
    LfvMaerkischerTurnerbundBrandenburg,
    LfvBremerTurnverband,
    LfvVerbandFuerTurnenUndFreizeitHamburg,
    LfvHessischerTurnverband,
    LfvTurnverbandMecklenburgVorpommern,
    LfvTurnverbandMittelrhein,
    LfvNiedersaechsischerTurnerBund,
    LfvPfaelzerTurnerbund,
    LfvRheinhessischerTurnerbund,
    LfvRheinischerTurnerbund,
    LfvSaarlaendischerTurnerbund,
    LfvSaechsischerTurnVerband,
    LfvLandesturnverbandSachsenAnhalt,
    LfvSchleswigHolsteinischerTurnverband,
    LfvSchwaebischerTurnerbund,
    LfvThueringerTurnverband,
    LfvWestfaelischerTurnerbund,
}

impl LandesturnverbandDTO {
    /// Every Landesturnverband, in the order the DTB lists them.
    pub const ALL: [Self; 20] = [
        Self::LfvBadischerTurnerBund,
        Self::LfvBayerischerTurnverband,
        Self::LfvBerlinerTurnUndFreizeitsportBund,
        Self::LfvMaerkischerTurnerbundBrandenburg,
        Self::LfvBremerTurnverband,
        Self::LfvVerbandFuerTurnenUndFreizeitHamburg,
        Self::LfvHessischerTurnverband,
        Self::LfvTurnverbandMecklenburgVorpommern,
        Self::LfvTurnverbandMittelrhein,
        Self::LfvNiedersaechsischerTurnerBund,
        Self::LfvPfaelzerTurnerbund,
        Self::LfvRheinhessischerTurnerbund,
        Self::LfvRheinischerTurnerbund,
        Self::LfvSaarlaendischerTurnerbund,
        Self::LfvSaechsischerTurnVerband,
        Self::LfvLandesturnverbandSachsenAnhalt,
        Self::LfvSchleswigHolsteinischerTurnverband,
        Self::LfvSchwaebischerTurnerbund,
        Self::LfvThueringerTurnverband,
        Self::LfvWestfaelischerTurnerbund,
    ];
}

impl OrganizationDTO {
    /// Every selectable organization: DTB, IRV, then all 20 Landesturnverbände.
    pub fn all() -> Vec<Self> {
        let mut out = vec![Self::DTB, Self::IRV];
        out.extend(LandesturnverbandDTO::ALL.iter().copied().map(Self::LFV));
        out
    }
}

impl Display for LandesturnverbandDTO {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let name = match self {
            Self::LfvBadischerTurnerBund => "Badischer Turner-Bund",
            Self::LfvBayerischerTurnverband => "Bayerischer Turnverband",
            Self::LfvBerlinerTurnUndFreizeitsportBund => "Berliner Turn- und Freizeitsport-Bund",
            Self::LfvMaerkischerTurnerbundBrandenburg => "Märkischer Turnerbund Brandenburg e.V.",
            Self::LfvBremerTurnverband => "Bremer Turnverband",
            Self::LfvVerbandFuerTurnenUndFreizeitHamburg => {
                "Verband für Turnen und Freizeit Hamburg"
            }
            Self::LfvHessischerTurnverband => "Hessischer Turnverband",
            Self::LfvTurnverbandMecklenburgVorpommern => "Turnverband Mecklenburg-Vorpommern",
            Self::LfvTurnverbandMittelrhein => "Turnverband Mittelrhein",
            Self::LfvNiedersaechsischerTurnerBund => "Niedersächsischer Turner-Bund",
            Self::LfvPfaelzerTurnerbund => "Pfälzer Turnerbund",
            Self::LfvRheinhessischerTurnerbund => "Rheinhessischer Turnerbund",
            Self::LfvRheinischerTurnerbund => "Rheinischer Turnerbund",
            Self::LfvSaarlaendischerTurnerbund => "Saarländischer Turnerbund",
            Self::LfvSaechsischerTurnVerband => "Sächsischer Turn-Verband",
            Self::LfvLandesturnverbandSachsenAnhalt => "Landesturnverband Sachsen-Anhalt",
            Self::LfvSchleswigHolsteinischerTurnverband => "Schleswig-Holsteinischer Turnverband",
            Self::LfvSchwaebischerTurnerbund => "Schwäbischer Turnerbund",
            Self::LfvThueringerTurnverband => "Thüringer Turnverband",
            Self::LfvWestfaelischerTurnerbund => "Westfälischer Turnerbund",
        };
        f.write_str(name)
    }
}
