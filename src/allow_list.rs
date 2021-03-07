use std::collections::HashSet;

use matrix_sdk::ruma::UserId;
use serde::Deserialize;

/// allow list storing matrix user ids and server names, we implement Deserialize our selves to validate that only valid matrix user identifiers and server names are used
#[derive(Debug, Clone)]
pub struct AllowList(HashSet<String>);

impl AllowList {
    pub fn contains(&self, user_id: &UserId) -> bool {
        self.0.contains(user_id.as_str()) || self.0.contains(user_id.server_name().as_str())
    }
}

struct AllowListVisitor();

impl<'de> serde::de::Visitor<'de> for AllowListVisitor {
    type Value = AllowList;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("`User Identifier` (https://spec.matrix.org/v1.2/appendices/#user-identifiers) or `Server Name` (https://spec.matrix.org/latest/appendices/#server-name)")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut set = HashSet::new();

        while let Some(str) = seq.next_element::<String>()? {
            if ruma_identifiers_validation::user_id::validate(&str)
                .or_else(|_| ruma_identifiers_validation::server_name::validate(&str))
                .is_ok()
            {
                set.insert(str);
            } else {
                return Err(serde::de::Error::invalid_type(
                    serde::de::Unexpected::Seq,
                    &self,
                ));
            }
        }

        Ok(AllowList(set))
    }
}

impl<'de> Deserialize<'de> for AllowList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(AllowListVisitor())
    }
}
