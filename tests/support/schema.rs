use tokio_postgres::Client;

pub async fn table_names(client: &Client) -> Vec<String> {
    client
        .query(
            "SELECT tablename::text FROM pg_tables WHERE schemaname = current_schema() ORDER BY 1",
            &[],
        )
        .await
        .expect("list the tables")
        .iter()
        .map(|row| row.get(0))
        .collect()
}
