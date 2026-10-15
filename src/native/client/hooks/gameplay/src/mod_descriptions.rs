//! C31: exact MP description keys, not equipment names, unlocks or save data.
//! Strings use the retail action token so rebound controls remain correct.
//! Unsupported locales/keys/sections/files always use native text.
pub(super) const SECTION: &str = "Character_Customization_Descriptions";
pub(super) const FILE: &str = "Localization\\Loc_Content";
pub(super) struct Entry {
    pub key: &'static str,
    pub en: &'static str,
    pub cht: &'static str,
}
macro_rules! entry {
    ($key:literal, $en:expr, $cht:expr) => {
        Entry {
            key: $key,
            en: $en,
            cht: $cht,
        }
    };
}
const INTEL_EN: &str = "PRESS {CtrlAct.ButtonsLarge.eActionType_ToggleArmorAbility} FOR <FONT COLOR=\"#6FDCB9\">INTEL</FONT>\nMARKS NEARBY ENEMIES AND REVEALS THEIR NAME AND HEALTH WHEN AIMED AT. MARKS RESIST DISRUPTOR AND END WHEN YOU DIE.";
const INTEL_CHT: &str = "按{CtrlAct.ButtonsLarge.eActionType_ToggleArmorAbility}使用<FONT COLOR=\"#6FDCB9\">情報標識</FONT>\n標記附近敵人，瞄準時顯示姓名與生命值。標記不受破壞者影響，施放者死亡時解除。";
const DISRUPTOR_EN: &str = "PRESS {CtrlAct.ButtonsLarge.eActionType_ToggleArmorAbility} FOR <FONT COLOR=\"#6FDCB9\">DISRUPTOR</FONT>\nTEMPORARILY DISABLES ENEMY GADGETS AND GOGGLES. INTEL MARKS REMAIN ACTIVE.";
const DISRUPTOR_CHT: &str = "按{CtrlAct.ButtonsLarge.eActionType_ToggleArmorAbility}使用<FONT COLOR=\"#6FDCB9\">破壞者</FONT>\n暫時癱瘓敵方裝置與戰術鏡；無法清除情報衣標記。";
const DRONE_EN: &str = "PRESS {CtrlAct.ButtonsLarge.eActionType_ToggleArmorAbility} TO DEPLOY OR LEAVE <FONT COLOR=\"#6FDCB9\">DRONE</FONT> CONTROL.\nMARKS VISIBLE TARGETS WITHIN 10 METRES IN CONTROL, 30 METRES AFTER EXIT. KEEPS USING BATTERY. RE-ENTER AFTER 3 SECONDS; REDEPLOY 15 SECONDS AFTER LOSS.";
const DRONE_CHT: &str = "按{CtrlAct.ButtonsLarge.eActionType_ToggleArmorAbility}部署或退出<FONT COLOR=\"#6FDCB9\">無人機</FONT>操控。\n操控時標記視野內10公尺的目標，退出後增至30公尺並持續耗電。3秒後可重入；無人機消失後15秒可再部署。";
pub(super) const ENTRIES: &[Entry] = &[
    entry!("LocID_CDESC_MPArmorBootsGhost",
        "MUFFLES MOVEMENT AND HALVES ATS MOVEMENT DETECTION RANGE. CROUCH-WALKING AVOIDS ATS MOVEMENT DETECTION.",
        "降低移動聲音，ATS移動偵測距離減半；穿戴時蹲下移動不會被ATS偵測。"),
    entry!("LocID_CDESC_MPArmorBootsGhost_Short", "QUIET MOVEMENT", "靜音移動"),
    entry!("LocID_CDESC_MPArmorPantsGhost",
        "DISGUISES CLOAK, INTEL AND OVERCHARGE SIGNALS AS FALSE RFD SWEEPS WITHIN 10 METRES, HIDING THEIR PRECISE LOCATION.",
        "將隱身、情報與超載的訊號偽裝成10公尺內的RFD假電流，隱藏準確位置。"),
    entry!("LocID_CDESC_MPArmorPantsGhost_Short", "RF DECEPTION", "射頻欺敵"),
    entry!("LocID_CDESC_MPArmorGlovesGhost",
        "INCREASES MELEE RANGE. TAKEDOWNS ARE 20% FASTER.",
        "增加近戰範圍，擊倒速度提高20%。"),
    entry!("LocID_CDESC_MPArmorGlovesGhost_Short", "MELEE RANGE / SPEED", "近戰範圍／速度"),
    entry!("LocID_CDESC_MPArmorHelmetTech",
        "RESISTS EMP DISRUPTION AND QUICKLY RESTORES GOGGLES AND ABILITIES.",
        "抵抗EMP干擾，迅速恢復戰術鏡與技能。"),
    entry!("LocID_CDESC_MPArmorHelmetTech_Short", "EMP COUNTER", "EMP反制"),
    entry!("LocID_CDESC_MPArmorTorsoIntelScout", INTEL_EN, INTEL_CHT),
    entry!("LocID_CDESC_MPArmorTorsoIntelScoutElite", INTEL_EN, INTEL_CHT),
    entry!("LocID_CDESC_MPArmorTorsoIntelScout_Power", INTEL_EN, INTEL_CHT),
    entry!("LocID_CDESC_MPArmorTorsoTech", DISRUPTOR_EN, DISRUPTOR_CHT),
    entry!("LocID_CDESC_MPArmorTorsoTechElite", DISRUPTOR_EN, DISRUPTOR_CHT),
    entry!("LocID_CDESC_MPArmorTorsoTech_Power", DISRUPTOR_EN, DISRUPTOR_CHT),
    entry!("LocID_CDESC_MPArmorTorsoHunter", DRONE_EN, DRONE_CHT),
    entry!("LocID_CDESC_MPArmorTorsoHunterElite", DRONE_EN, DRONE_CHT),
    entry!("LocID_CDESC_MPArmorTorsoHunter_Power", DRONE_EN, DRONE_CHT),
    entry!("LocID_CDESC_GI_MP_BatsVision",
        "DETECTS MOVEMENT WITHIN 20 METRES AND TRACKS UNSUPPRESSED GUNFIRE. SUPPRESSED PRIMARY FIRE WITHIN 20 METRES SHARES THE LAST FIRING POSITION WITH YOUR TEAM FOR 5 SECONDS.",
        "偵測20公尺內的移動並追蹤未消音槍聲。20公尺內的消音主武器開火時，全隊可看到最後開火位置，持續5秒。"),
    entry!("LocID_CDESC_GI_MP_BMSensor",
        "DETECTS FAST-MOVING ENEMIES; CROUCHING OR HANGING AVOIDS DETECTION. AIM AT ENEMIES TO REVEAL NAME AND HEALTH.",
        "偵測快速移動的敵人；蹲下或懸掛可避開偵測。瞄準敵人時顯示姓名與生命值。"),
    entry!("LocID_CDESC_GI_MP_RFDetector",
        "LOCATES ENEMIES USING ELECTRONIC EQUIPMENT. ID BLOCKER PANTS CAN DISGUISE SUIT ABILITY SIGNALS.",
        "偵測使用電子裝備的敵人位置。分身阻擋褲可偽裝衣服技能訊號。"),
];

pub(super) fn lookup(
    enabled: bool,
    section: &str,
    key: &str,
    file: &str,
    language: &str,
) -> Option<&'static str> {
    if !enabled || section != SECTION || file != FILE {
        return None;
    }
    let e = ENTRIES.iter().find(|e| e.key == key)?;
    if language.eq_ignore_ascii_case("int") {
        Some(e.en)
    } else if language.eq_ignore_ascii_case("cht") {
        Some(e.cht)
    } else {
        None
    }
}
