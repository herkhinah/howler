use std::str::FromStr;

use anyhow::{Context, Result};
use matrix_sdk::ruma::identifiers::RoomId;
use once_cell::sync::OnceCell;
use rand::Rng;
use sled::Transactional;

use crate::settings::Settings;

static ROOM_TOKENS: OnceCell<RoomTokenMap> = OnceCell::new();

#[derive(Clone, Debug)]
pub struct RoomTokenMap {
    room_id_token_map: sled::Tree,
    token_room_id_map: sled::Tree,
}

impl RoomTokenMap {
    fn new(db: sled::Db) -> Self {
        Self {
            room_id_token_map: db.open_tree("token_user_id_map").unwrap(),
            token_room_id_map: db.open_tree("user_id_token_map").unwrap(),
        }
    }

    pub fn global() -> &'static Self {
        ROOM_TOKENS.get_or_init(|| {
            let mut path = std::path::PathBuf::from(&Settings::global().store_path);
            std::fs::create_dir_all(&path)
                .context(format!("could not create path {:?}", path))
                .unwrap();
            path.push("sled.db");

            let db = sled::open(&path).unwrap();

            RoomTokenMap::new(db)
        })
    }

    pub fn get_or_create_token(&self, room_id: &RoomId) -> Result<String> {
        if let Some(token) = self.get_token(room_id)? {
            return Ok(token);
        }

        let token = {
            let mut random = [0u8; 64];
            rand::thread_rng().fill(&mut random);
            base64::encode_config(random, base64::URL_SAFE_NO_PAD)
        };

        (&self.room_id_token_map, &self.token_room_id_map)
            .transaction(|(room_id_token_map, token_room_id_map)| -> std::result::Result<(), sled::transaction::ConflictableTransactionError>  {
                room_id_token_map.insert(room_id.as_str(), token.as_str())?;
                token_room_id_map.insert(token.as_str(), room_id.as_str())?;
                Ok(())
            }).context("sled internal error")?;

        Ok(token)
    }

    pub fn get_token(&self, room_id: &RoomId) -> Result<Option<String>> {
        match self
            .room_id_token_map
            .get(room_id.as_str())
            .context("sled internal error")?
        {
            Some(token) => Ok(Some(String::from_utf8(token.to_vec()).unwrap())),
            None => Ok(None),
        }
    }

    pub fn get_room_id(&self, token: &str) -> Result<Option<RoomId>> {
        match self
            .token_room_id_map
            .get(token)
            .context("sled internal error")?
        {
            Some(room_id) => Ok(Some(
                RoomId::from_str(std::str::from_utf8(&room_id).unwrap()).unwrap(),
            )),
            None => Ok(None),
        }
    }
}
