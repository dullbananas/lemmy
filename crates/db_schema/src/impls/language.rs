use crate::{
  diesel::ExpressionMethods,
  newtypes::LanguageId,
  schema::language::dsl::{code, id, language},
  source::language::Language,
  utils::DbConn,
};
use diesel::{result::Error, QueryDsl};
use diesel_async::{AsyncPgConnection, RunQueryDsl};

impl Language {
  pub async fn read_all(conn: &mut DbConn) -> Result<Vec<Language>, Error> {
    Self::read_all_conn(conn).await
  }

  pub async fn read_all_conn(conn: &mut AsyncPgConnection) -> Result<Vec<Language>, Error> {
    language.load::<Self>(conn).await
  }

  pub async fn read_from_id(conn: &mut DbConn, id_: LanguageId) -> Result<Language, Error> {
    language.filter(id.eq(id_)).first::<Self>(conn).await
  }

  /// Attempts to find the given language code and return its ID. If not found, returns none.
  pub async fn read_id_from_code(
    conn: &mut DbConn,
    code_: Option<&str>,
  ) -> Result<Option<LanguageId>, Error> {
    if let Some(code_) = code_ {
      Ok(
        language
          .filter(code.eq(code_))
          .first::<Self>(conn)
          .await
          .map(|l| l.id)
          .ok(),
      )
    } else {
      Ok(None)
    }
  }
}

#[cfg(test)]
mod tests {
  use crate::{source::language::Language, utils::build_db_conn_for_tests};
  use serial_test::serial;

  #[tokio::test]
  #[serial]
  async fn test_languages() {
    let conn = &mut build_db_conn_for_tests().await;

    let all = Language::read_all(conn).await.unwrap();

    assert_eq!(184, all.len());
    assert_eq!("ak", all[5].code);
    assert_eq!("lv", all[99].code);
    assert_eq!("yi", all[179].code);
  }
}
