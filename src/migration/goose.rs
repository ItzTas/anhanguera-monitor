use std::collections::HashSet;

use sea_orm::{ConnectionTrait, DbErr, Statement};

const GOOSE_FILES: [(i64, &str); 2] = [
    (1, include_str!("../../sql/schema/00001_init.sql")),
    (
        2,
        include_str!("../../sql/schema/00002_invoice_per_enrollment.sql"),
    ),
];

const TABLE_EXISTS: &str = "SELECT EXISTS ( SELECT 1 FROM pg_tables WHERE (current_schema() IS NULL OR schemaname = current_schema()) AND tablename = 'goose_db_version' )";
const LIST_VERSIONS: &str = "SELECT version_id FROM goose_db_version ORDER BY id DESC";

#[derive(Debug, thiserror::Error)]
pub(crate) enum GooseError {
    #[error(
        "migration version {missing} is not applied but the database is already at version {current}"
    )]
    OutOfOrder { missing: i64, current: i64 },
    #[error("goose_db_version has no rows, not even the version 0 row goose expects")]
    MissingZeroVersion,
    #[error(transparent)]
    Database(#[from] DbErr),
}

pub(crate) fn up_section(file: &str) -> &str {
    file.split_once("-- +goose Down").map_or(file, |(up, _)| up)
}

pub(crate) async fn catch_up(db: &impl ConnectionTrait) -> Result<(), GooseError> {
    let recorded = recorded_versions(db).await?;
    let current = recorded.iter().copied().max().unwrap_or(0);
    let pending: Vec<(i64, &str)> = GOOSE_FILES
        .into_iter()
        .filter(|(version, _)| !recorded.contains(version))
        .collect();
    if let Some(&(missing, _)) = pending.iter().find(|(version, _)| *version < current) {
        return Err(GooseError::OutOfOrder { missing, current });
    }
    for (_, file) in pending {
        db.execute_unprepared(up_section(file)).await?;
    }
    Ok(())
}

async fn recorded_versions(db: &impl ConnectionTrait) -> Result<HashSet<i64>, GooseError> {
    let backend = db.get_database_backend();
    let exists = db
        .query_one_raw(Statement::from_string(backend, TABLE_EXISTS))
        .await?
        .ok_or_else(|| DbErr::Custom("the goose_db_version check returned no row".to_owned()))?
        .try_get_by_index::<bool>(0)?;
    if !exists {
        return Ok(HashSet::new());
    }
    let rows = db
        .query_all_raw(Statement::from_string(backend, LIST_VERSIONS))
        .await?;
    if rows.is_empty() {
        return Err(GooseError::MissingZeroVersion);
    }
    rows.iter()
        .map(|row| Ok(row.try_get_by_index::<i64>(0)?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_sections_stop_before_the_down_section() {
        let [(1, init), (2, per_enrollment)] = GOOSE_FILES else {
            panic!("the goose files are versions 1 and 2");
        };
        let init = up_section(init);
        assert!(init.contains("CREATE TABLE monitor_state"), "{init}");
        assert!(!init.contains("DROP TABLE"), "{init}");
        let per_enrollment = up_section(per_enrollment);
        assert!(
            per_enrollment.contains("ADD PRIMARY KEY (course_enrollment_id, id)"),
            "{per_enrollment}"
        );
        assert!(!per_enrollment.contains("DELETE FROM"), "{per_enrollment}");
    }
}
