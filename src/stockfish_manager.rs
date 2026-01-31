use stockfish::Stockfish;
use deadpool::managed::{Manager, Metrics, RecycleResult};
use std::io;
use std::future::Future;

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
            tokio::task::spawn_blocking(move || {
                Stockfish::new(&path).map_err(|e| {
                    io::Error::new(io::ErrorKind::Other, format!("Failed to create Stockfish: {}", e))
                })
            })
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Spawn error: {}", e)))?
        }
    }

    fn recycle(
        &self,
        obj: &mut Self::Type,
        _metrics: &Metrics,
    ) -> impl Future<Output = RecycleResult<Self::Error>> + Send {
        async move {
            obj.setup_for_new_game()
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Recycle error: {}", e)))?;
            
            Ok(())
        }
    }
}
