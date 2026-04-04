pub mod models;
pub mod repo;

use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnectOptions;

pub async fn init_pool(database_url: &str) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::new()
        .filename(database_url)
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(options).await?;
    run_migrations(&pool).await?;
    Ok(pool)
}

pub async fn run_migrations(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(include_str!("../../migrations/001_initial.sql"))
        .execute(pool)
        .await?;
    Ok(())
}
