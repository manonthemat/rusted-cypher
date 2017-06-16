//! Transaction management through neo4j's transaction endpoint
//!
//! The recommended way to start a transaction is through the `GraphClient`
//!
//! # Examples
//!
//! ## Starting a transaction
//! ```
//! # use rusted_cypher::{GraphClient, GraphError};
//! # const URL: &'static str = "http://neo4j:neo4j@localhost:7474/db/data";
//! # fn main() { doctest().unwrap(); }
//! # #[allow(unused_variables)]
//! # fn doctest() -> Result<(), GraphError> {
//! # let graph = GraphClient::connect(URL)?;
//! let mut transaction = graph.transaction();
//! transaction.add_statement("MATCH (n:TRANSACTION) RETURN n");
//!
//! let (transaction, results) = transaction.begin()?;
//! # transaction.rollback()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Statement is optional when beggining a transaction
//! ```
//! # use rusted_cypher::{GraphClient, GraphError};
//! # const URL: &'static str = "http://neo4j:neo4j@localhost:7474/db/data";
//! # fn main() { doctest().unwrap(); }
//! # #[allow(unused_variables)]
//! # fn doctest() -> Result<(), GraphError> {
//! # let graph = GraphClient::connect(URL)?;
//! let (transaction, _) = graph.transaction().begin()?;
//! # transaction.rollback()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Send queries in a started transaction
//! ```
//! # use rusted_cypher::{GraphClient, GraphError};
//! # const URL: &'static str = "http://neo4j:neo4j@localhost:7474/db/data";
//! # fn main() { doctest().unwrap(); }
//! # fn doctest() -> Result<(), GraphError> {
//! # let graph = GraphClient::connect(URL)?;
//! # let (mut transaction, _) = graph.transaction().begin()?;
//! // Send a single query
//! let result = transaction.exec("MATCH (n:TRANSACTION) RETURN n")?;
//!
//! // Send multiple queries
//! let results = transaction
//!     .with_statement("MATCH (n:TRANSACTION) RETURN n")
//!     .with_statement("MATCH (n:OTHER_TRANSACTION) RETURN n")
//!     .send()?;
//! # assert_eq!(results.len(), 2);
//! # transaction.rollback()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Commit a transaction
//! ```
//! # use rusted_cypher::{GraphClient, GraphError};
//! # const URL: &'static str = "http://neo4j:neo4j@localhost:7474/db/data";
//! # fn main() { doctest().unwrap(); }
//! # fn doctest() -> Result<(), GraphError> {
//! # let graph = GraphClient::connect(URL)?;
//! # let (mut transaction, _) = graph.transaction().begin()?;
//! transaction.exec("CREATE (n:TRANSACTION)")?;
//! transaction.commit()?;
//!
//! // Send more statements when commiting
//! # let (mut transaction, _) = graph.transaction().begin()?;
//! let results = transaction.with_statement(
//!     "MATCH (n:TRANSACTION) RETURN n")
//!     .send()?;
//! # assert_eq!(results[0].data.len(), 1);
//! # transaction.rollback()?;
//! # graph.exec("MATCH (n:TRANSACTION) DELETE n")?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Rollback a transaction
//! ```
//! # use rusted_cypher::{GraphClient, GraphError};
//! # const URL: &'static str = "http://neo4j:neo4j@localhost:7474/db/data";
//! # fn main() { doctest().unwrap(); }
//! # fn doctest() -> Result<(), GraphError> {
//! # let graph = GraphClient::connect(URL)?;
//! # let (mut transaction, _) = graph.transaction().begin()?;
//! transaction.exec("CREATE (n:TRANSACTION)")?;
//! transaction.rollback()?;
//! # let result = graph.exec("MATCH (n:TRANSACTION) RETURN n")?;
//! # assert_eq!(result.data.len(), 0);
//! # Ok(())
//! # }
//! ```

use std::any::Any;
use std::marker::PhantomData;
use std::mem;
use hyper::Client;
use hyper::header::{Headers, Location};
use time::{self, Tm};

use error::{GraphError, Neo4jError};
use super::result::{CypherResult, ResultTrait};
use super::statement::Statement;

const DATETIME_RFC822: &'static str = "%a, %d %b %Y %T %Z";

pub struct Created;
pub struct Started;

#[derive(Debug, Deserialize)]
struct TransactionInfo {
    expires: String,
}

#[derive(Debug, Deserialize)]
struct TransactionResult {
    commit: String,
    transaction: TransactionInfo,
    results: Vec<CypherResult>,
    errors: Vec<Neo4jError>,
}

impl ResultTrait for TransactionResult {
    fn results(&self) -> &Vec<CypherResult> {
        &self.results
    }

    fn errors(&self) -> &Vec<Neo4jError> {
        &self.errors
    }
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct CommitResult {
    results: Vec<CypherResult>,
    errors: Vec<Neo4jError>,
}

impl ResultTrait for CommitResult {
    fn results(&self) -> &Vec<CypherResult> {
        &self.results
    }

    fn errors(&self) -> &Vec<Neo4jError> {
        &self.errors
    }
}

/// Provides methods to interact with a transaction
///
/// This struct is used to begin a transaction, send queries, commit an rollback a transaction.
/// Some methods are provided depending on the state of the transaction, for example,
/// `Transaction::begin` is provided on a `Created` transaction and `Transaction::commit` is provided
/// on `Started` transaction
pub struct Transaction<'a, State: Any = Created> {
    transaction: String,
    commit: String,
    expires: Tm,
    client: Client,
    headers: &'a Headers,
    statements: Vec<Statement>,
    _state: PhantomData<State>,
}

impl<'a, State: Any> Transaction<'a, State> {
    /// Adds a statement to the transaction
    pub fn add_statement<S: Into<Statement>>(&mut self, statement: S) {
        self.statements.push(statement.into());
    }

    /// Gets the expiration time of the transaction
    pub fn get_expires(&self) -> &Tm {
        &self.expires
    }
}

impl<'a> Transaction<'a, Created> {
    pub fn new(endpoint: &str, headers: &'a Headers) -> Transaction<'a, Created> {
        Transaction {
            transaction: endpoint.to_owned(),
            commit: endpoint.to_owned(),
            expires: time::now_utc(),
            client: Client::new(),
            headers: headers,
            statements: vec![],
            _state: PhantomData,
        }
    }

    /// Adds a statement to the transaction in builder style
    pub fn with_statement<S: Into<Statement>>(mut self, statement: S) -> Self {
        self.add_statement(statement);
        self
    }

    /// Begins the transaction
    ///
    /// Consumes the `Transaction<Created>` and returns the a `Transaction<Started>` alongside with
    /// the results of any `Statement` sent.
    pub fn begin(self) -> Result<(Transaction<'a, Started>, Vec<CypherResult>), GraphError> {
        debug!("Beginning transaction");

        let mut res = super::send_query(&self.client,
                                        &self.transaction,
                                        self.headers,
                                        self.statements)?;

        let result: TransactionResult = super::parse_response(&mut res)?;

        let transaction = res.headers
            .get::<Location>()
            .map(|location| location.0.to_owned())
            .ok_or_else(|| {
                            error!("No transaction URI returned from server");
                            GraphError::Transaction("No transaction URI returned from server"
                                                        .to_owned())
                        })?;

        let expires = time::strptime(&result.transaction.expires, DATETIME_RFC822)?;

        debug!("Transaction started at {}, expires in {}",
               transaction,
               expires.rfc822z());

        let transaction = Transaction {
            transaction: transaction,
            commit: result.commit,
            expires: expires,
            client: self.client,
            headers: self.headers,
            statements: Vec::new(),
            _state: PhantomData,
        };

        Ok((transaction, result.results))
    }
}

impl<'a> Transaction<'a, Started> {
    /// Adds a statement to the transaction in builder style
    pub fn with_statement<S: Into<Statement>>(&mut self, statement: S) -> &mut Self {
        self.add_statement(statement);
        self
    }

    /// Executes the given statement
    ///
    /// Any statements added via `add_statement` or `with_statement` will be discarded
    pub fn exec<S: Into<Statement>>(&mut self, statement: S) -> Result<CypherResult, GraphError> {
        self.statements.clear();
        self.add_statement(statement);

        let mut results = self.send()?;
        let result =
            results.pop()
                .ok_or_else(|| GraphError::Statement("Server returned no results".to_owned()))?;

        Ok(result)
    }

    /// Executes the statements added via `add_statement` or `with_statement`
    pub fn send(&mut self) -> Result<Vec<CypherResult>, GraphError> {
        let mut statements = vec![];
        mem::swap(&mut statements, &mut self.statements);
        let mut res = super::send_query(&self.client, &self.transaction, self.headers, statements)?;

        let result: TransactionResult = super::parse_response(&mut res)?;
        self.expires = time::strptime(&result.transaction.expires, DATETIME_RFC822)?;

        Ok(result.results)
    }

    /// Commits the transaction, returning the results
    pub fn commit(self) -> Result<Vec<CypherResult>, GraphError> {
        debug!("Commiting transaction {}", self.transaction);
        let mut res = super::send_query(&self.client, &self.commit, self.headers, self.statements)?;

        let result: CommitResult = super::parse_response(&mut res)?;
        debug!("Transaction commited {}", self.transaction);

        Ok(result.results)
    }

    /// Rollback the transaction
    pub fn rollback(self) -> Result<(), GraphError> {
        debug!("Rolling back transaction {}", self.transaction);
        let mut res = self.client
            .delete(&self.transaction)
            .headers(self.headers.clone())
            .send()?;

        super::parse_response::<CommitResult>(&mut res)?;
        debug!("Transaction rolled back {}", self.transaction);

        Ok(())
    }

    /// Sends a query to just reset the transaction timeout
    ///
    /// All transactions have a timeout. Use this method to keep a transaction alive.
    pub fn reset_timeout(&mut self) -> Result<(), GraphError> {
        super::send_query(&self.client, &self.transaction, self.headers, vec![]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::{Authorization, Basic, ContentType, Headers};

    const URL: &'static str = "http://neo4j:neo4j@localhost:7474/db/data/transaction";

    fn get_headers() -> Headers {
        let mut headers = Headers::new();

        headers.set(Authorization(Basic {
                                      username: "neo4j".to_owned(),
                                      password: Some("neo4j".to_owned()),
                                  }));

        headers.set(ContentType::json());

        headers
    }

    #[test]
    fn begin_transaction() {
        let headers = get_headers();
        let transaction = Transaction::new(URL, &headers);
        let result = transaction.begin().unwrap();
        assert_eq!(result.1.len(), 0);
    }

    #[test]
    fn create_node_and_commit() {
        let headers = get_headers();

        Transaction::new(URL, &headers)
            .with_statement("CREATE (n:TEST_TRANSACTION_CREATE_COMMIT { name: 'Rust', safe: true })")
            .begin().unwrap()
            .0.commit().unwrap();

        let (transaction, results) = Transaction::new(URL, &headers)
            .with_statement("MATCH (n:TEST_TRANSACTION_CREATE_COMMIT) RETURN n")
            .begin()
            .unwrap();

        assert_eq!(results[0].data.len(), 1);

        transaction.rollback().unwrap();

        Transaction::new(URL, &headers)
            .with_statement("MATCH (n:TEST_TRANSACTION_CREATE_COMMIT) DELETE n")
            .begin()
            .unwrap()
            .0
            .commit()
            .unwrap();
    }

    #[test]
    fn create_node_and_rollback() {
        let headers = get_headers();

        let (mut transaction, _) = Transaction::new(URL, &headers)
            .with_statement("CREATE (n:TEST_TRANSACTION_CREATE_ROLLBACK { name: 'Rust', safe: true })")
            .begin().unwrap();

        let result = transaction
            .exec("MATCH (n:TEST_TRANSACTION_CREATE_ROLLBACK) RETURN n")
            .unwrap();

        assert_eq!(result.data.len(), 1);

        transaction.rollback().unwrap();

        let (transaction, results) = Transaction::new(URL, &headers)
            .with_statement("MATCH (n:TEST_TRANSACTION_CREATE_ROLLBACK) RETURN n")
            .begin()
            .unwrap();

        assert_eq!(results[0].data.len(), 0);

        transaction.rollback().unwrap();
    }

    #[test]
    fn query_open_transaction() {
        let headers = get_headers();

        let (mut transaction, _) = Transaction::new(URL, &headers).begin().unwrap();

        let result = transaction
            .exec("CREATE (n:TEST_TRANSACTION_QUERY_OPEN { name: 'Rust', safe: true }) RETURN n")
            .unwrap();

        assert_eq!(result.data.len(), 1);

        transaction.rollback().unwrap();
    }
}
