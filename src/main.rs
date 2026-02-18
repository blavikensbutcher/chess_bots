use dotenv::dotenv;
use shakmaty::fen::Fen;
use shakmaty::{san::San, uci::UciMove, Chess};
use tonic::{transport::Server, Request, Response, Status};
mod config;
mod stockfish_manager;
use deadpool::managed::Pool;
use std::net::SocketAddr;
use stockfish_manager::StockfishManager;

pub mod chess_bot {
    tonic::include_proto!("chess_bot");
}

use chess_bot::chess_bot_server::{ChessBot, ChessBotServer};
use chess_bot::{MoveResponse, PositionRequest};
use config::Config;

#[derive(Clone)]
pub struct ChessBotService {
    pool: Pool<StockfishManager>,
}

#[tonic::async_trait]
impl ChessBot for ChessBotService {
    async fn get_best_move(
        &self,
        request: Request<PositionRequest>,
    ) -> Result<Response<MoveResponse>, Status> {
        let req = request.into_inner();

        println!(
            "📥 Received request: FEN={}, ELO={}",
            &req.fen[..30],
            req.elo_rating
        );
        
        let mut stockfish = self.pool.get().await.map_err(|e| {
            eprintln!("❌ Failed to get Stockfish from pool: {}", e);
            Status::internal("Pool exhausted")
        })?;

        println!("✅ Got Stockfish from pool");

        let skill_level = calculate_skill_from_elo(req.elo_rating);
        let depth = calculate_depth_from_elo(req.elo_rating);

        println!("🎯 Skill level: {}, depth: {}", skill_level, depth);

    
        let fen = req.fen.clone();
        let result = tokio::task::spawn_blocking(move || {
            // Skill level setup
            stockfish
                .uci_send(&format!("setoption name Skill Level value {}", skill_level))
                .map_err(|e| format!("Skill setup error: {}", e))?;

            // Position setup
            stockfish
                .set_fen_position(&fen)
                .map_err(|e| format!("Invalid FEN: {}", e))?;

            // Calculating best move
            stockfish.set_depth(depth as u32);
            let engine_result = stockfish.go().map_err(|e| format!("Engine error: {}", e))?;

            Ok::<_, String>(engine_result)
        })
        .await
        .map_err(|e| Status::internal(format!("Spawn error: {}", e)))?
        .map_err(|e| Status::internal(e))?;

        println!("✅ Got best move: {}", result.best_move());

        let uci_move_str = result.best_move().to_string();

        let fen: Fen = req
            .fen
            .parse()
            .map_err(|e| Status::invalid_argument(format!("Invalid FEN: {:?}", e)))?;

        let pos: Chess = fen
            .into_position(shakmaty::CastlingMode::Standard)
            .map_err(|e| Status::invalid_argument(format!("Invalid position: {:?}", e)))?;

        let uci_move: UciMove = uci_move_str
            .parse()
            .map_err(|e| Status::internal(format!("Invalid UCI move: {:?}", e)))?;

        let chess_move = uci_move
            .to_move(&pos)
            .map_err(|e| Status::internal(format!("Illegal move: {:?}", e)))?;

        let (from, to, piece, captured, promotion) = match &chess_move {
            shakmaty::Move::Normal {
                role,
                from,
                to,
                capture,
                promotion,
            } => {
                let piece_name = format!("{:?}", role);
                let captured_name = capture.map(|c| format!("{:?}", c));
                let promotion_name = promotion.map(|p| format!("{:?}", p));

                (
                    from.to_string(),
                    to.to_string(),
                    piece_name,
                    captured_name,
                    promotion_name,
                )
            }
            shakmaty::Move::Castle { king, rook } => {
                use shakmaty::{File, Square};

                let king_to = if rook.file() == File::A {
                    Square::from_coords(File::C, king.rank())
                } else {
                    Square::from_coords(File::G, king.rank())
                };

                (
                    king.to_string(),
                    king_to.to_string(),
                    "King".to_string(),
                    None,
                    None,
                )
            }
            shakmaty::Move::EnPassant { from, to } => (
                from.to_string(),
                to.to_string(),
                "Pawn".to_string(),
                Some("Pawn".to_string()),
                None,
            ),
            shakmaty::Move::Put { .. } => {
                return Err(Status::internal("Put move not supported"));
            }
        };

        let san = San::from_move(&pos, chess_move).to_string();

        println!("📤 Sending response: {}", san);

        let response = MoveResponse {
            best_move: uci_move_str,
            score: result.eval().value(),
            from,
            to,
            piece,
            captured,
            promotion,
            san,
        };

        Ok(Response::new(response))
    }
}

fn calculate_skill_from_elo(elo: i32) -> i32 {
    match elo {
        ..=1349 => 1,
        1350..=1549 => 2,
        1550..=1649 => 3,
        1650..=1749 => 4,
        1750..=1849 => 5,
        1850..=1949 => 6,
        1950..=2049 => 7,
        2050..=2149 => 8,
        2150..=2249 => 9,
        2250..=2349 => 10,
        2350..=2449 => 11,
        2450..=2549 => 12,
        2550..=2649 => 13,
        2650..=2749 => 14,
        2750..=2849 => 15,
        2850..=2949 => 16,
        2950..=3049 => 17,
        _ => 18,
    }
}

fn calculate_depth_from_elo(elo: i32) -> u8 {
    match elo {
        ..=1349 => 1,
        1350..=1549 => 2,
        1550..=1649 => 3,
        1650..=1749 => 4,
        1750..=1849 => 5,
        1850..=1949 => 6,
        1950..=2049 => 7,
        2050..=2149 => 8,
        2150..=2249 => 9,
        2250..=2349 => 10,
        2350..=2449 => 11,
        2450..=2549 => 12,
        2550..=2649 => 13,
        2650..=2749 => 14,
        2750..=2849 => 15,
        2850..=2949 => 16,
        2950..=3049 => 17,
        _ => 18,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();
    let config = Config::from_env().expect("Failed to load configuration");

    let stockfish_path =
        std::env::var("STOCKFISH_PATH").unwrap_or_else(|_| "/usr/games/stockfish".to_string());

    println!("🔧 Creating Stockfish pool...");

    let manager = StockfishManager::new(stockfish_path);
    let pool = Pool::builder(manager)
        .max_size(num_cpus::get() as usize) 
        .build()
        .map_err(|e| format!("Failed to create pool: {}", e))?;

    println!("✅ Stockfish pool created with {} instances", num_cpus::get());

    let bot_service = ChessBotService { pool };

    let host = &config.server_host;
    let port: u16 = config.server_port;
    let addr = SocketAddr::new(host.parse()?, port);

    println!(
        "Chess Bot gRPC Server listening on {}",
        config.server_address()
    );

    Server::builder()
        .add_service(ChessBotServer::new(bot_service))
        .serve(addr)
        .await?;

    Ok(())
}
