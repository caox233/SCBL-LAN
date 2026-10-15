use std::collections::HashSet;

use super::run;
use super::Result;
use super::Storage;

impl Storage {
    pub fn register_urls(&self, user_id: u32, urls: Vec<String>) -> Result<()> {
        let urls = urls.into_iter().collect::<HashSet<_>>();
        run(async {
            let mut transaction = self.begin_write().await?;
            sqlx::query("DELETE FROM station_urls WHERE user_id = ?").bind(user_id).execute(&mut *transaction).await?;
            for url in urls {
                sqlx::query("INSERT OR IGNORE INTO station_urls (user_id, url) VALUES (?, ?)")
                    .bind(user_id)
                    .bind(url)
                    .execute(&mut *transaction)
                    .await?;
            }
            transaction.commit().await
        })??;
        Ok(())
    }

    pub async fn list_urls(&self, user_id: u32) -> Result<Vec<String>> {
        Ok(sqlx::query_as("SELECT url FROM station_urls WHERE user_id = ?")
            .bind(user_id)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|r: (String,)| r.0)
            .collect())
    }
}
