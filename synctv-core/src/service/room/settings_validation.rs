use crate::{
    Result,
    models::{RoomSettings, SettingsValidationContext},
};

use super::RoomService;

impl RoomService {
    fn validate_settings(&self, settings: &RoomSettings) -> Result<()> {
        if let Some(runtime_settings_store) = self.runtime_settings_store.as_ref() {
            return settings.validate(&runtime_settings_store.validation_context());
        }
        SettingsValidationContext::with_strict_policy(|ctx| settings.validate(ctx))
    }

    pub(super) fn validate_room_settings(&self, settings: &RoomSettings) -> Result<()> {
        self.validate_settings(settings)
    }
}
