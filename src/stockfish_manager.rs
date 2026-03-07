use deadpool::managed::{Manager, Metrics, RecycleResult};
use std::future::Future;
use std::io;
use stockfish::Stockfish;

pub struct StockfishManager {
    path: String,
}

impl StockfishManager {
    pub fn new(path: String) -> Self {
        Self { path }
    }
}

impl Manager for StockfishManager {
    type Type = Stockfish;
    type Error = io::Error;

    fn create(&self) -> impl Future<Output = Result<Self::Type, Self::Error>> + Send {
        let path = self.path.clone();

        async move {
            let mut engine = tokio::task::spawn_blocking(move || {
                Stockfish::new(&path).map_err(|e| {
                    io::Error::other(format!("Failed to create Stockfish: {}", e))
                })
            })
            .await
            .map_err(|e| io::Error::other(format!("Spawn error: {}", e)))??;

            engine
                .uci_send("setoption name UCI_Variant value chess")
                .map_err(|e| io::Error::other(format!("UCI_Variant setup error: {}", e)))?;

            Ok(engine)
        }
    }

    fn recycle(
        &self,
        obj: &mut Self::Type,
        _metrics: &Metrics,
    ) -> impl Future<Output = RecycleResult<Self::Error>> + Send {
        async move {
            obj.setup_for_new_game()
                .map_err(|e| io::Error::other(format!("Recycle error: {}", e)))?;

            obj.uci_send("setoption name UCI_Variant value chess")
                .map_err(|e| io::Error::other(format!("UCI_Variant recycle error: {}", e)))?;

            Ok(())
        }
    }
}
