use serde::{Deserialize, Serialize};
use surrealdb::sql::Thing;

#[derive(Serialize, Deserialize, Clone)]
pub struct SiteURLNode {
    pub url: String,
    pub id: Option<Thing>,
}

impl SiteURLNode {
    pub fn new(url: String) -> Self {
        Self {
            url,
            id: None,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Record {
    pub id: Thing,
}