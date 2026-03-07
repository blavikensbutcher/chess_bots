use deadpool::managed::Pool;
use dotenv::dotenv;
use rand::prelude::IndexedRandom; // Оновлений імпорт для rand v0.10.0
use rand::RngExt;
use shakmaty::fen::Fen;
use shakmaty::{san::San, uci::UciMove, Chess, Position};
use std::net::SocketAddr;
use tonic::{transport::Server, Request, Response, Status};

mod config;
mod stockfish_manager;
use config::Config;
use stockfish_manager::StockfishManager;

pub mod chess_bot {
    tonic::include_proto!("chess_bot");
}

use chess_bot::chess_bot_server::{ChessBot, ChessBotServer};
use chess_bot::{MoveResponse, PositionRequest};

#[derive(Clone)]
pub struct ChessBotService {
    pool: Pool<StockfishManager>,
}

fn extract_move_details(
    chess_move: &shakmaty::Move,
) -> Result<
    (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
    ),
    Status,
> {
    match chess_move {
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

            Ok((
                from.to_string(),
                to.to_string(),
                piece_name,
                captured_name,
                promotion_name,
            ))
        }
        shakmaty::Move::Castle { king, rook } => {
            use shakmaty::{File, Square};

            let king_to = if rook.file() == File::A {
                Square::from_coords(File::C, king.rank())
            } else {
                Square::from_coords(File::G, king.rank())
            };

            Ok((
                king.to_string(),
                king_to.to_string(),
                "King".to_string(),
                None,
                None,
            ))
        }
        shakmaty::Move::EnPassant { from, to } => Ok((
            from.to_string(),
            to.to_string(),
            "Pawn".to_string(),
            Some("Pawn".to_string()),
            None,
        )),
        shakmaty::Move::Put { .. } => {
            Err(Status::internal("Put move not supported"))
        }
    }
}

fn calculate_skill_from_elo(elo: i32) -> i32 {
    match elo {
        ..=1199 => 0,      // Minimum skill for < 1200
        1200..=1349 => 1,
        1350..=1549 => 2,
        1550..=1649 => 3,
        1650..=1749 => 4,
        1750..=1849 => 5,
        1850..=1949 => 6,
        1950..=2049 => 8,
        2050..=2149 => 10,
        2150..=2249 => 12,
        2250..=2349 => 14,
        2350..=2449 => 16,
        2450..=2549 => 17,
        2550..=2649 => 18,
        2650..=2749 => 19,
        _ => 20,           // Max skill level for Stockfish is 20
    }
}

fn calculate_depth_from_elo(elo: i32) -> u8 {
    match elo {
        ..=1999 => 5,  
        2000..=2199 => 6,
        2200..=2399 => 8,   // Lichess Level 6 = depth 8
        2400..=2599 => 10,
        2600..=2799 => 13,  // Lichess Level 7 = depth 13
        _ => 22,            // Lichess Level 8 = depth 22
    }
}

fn calculate_blunder_chance(elo: i32) -> f64 {
    match elo {
        ..=600 => 0.40,   // 40% chance of a random move
        601..=800 => 0.35,
        801..=1000 => 0.25,
        1001..=1199 => 0.15,
        _ => 0.0,
    }
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

        // 1. Parse FEN and create position first
        let fen: Fen = req
            .fen
            .parse()
            .map_err(|e| Status::invalid_argument(format!("Invalid FEN: {:?}", e)))?;

        let pos: Chess = fen
            .clone()
            .into_position(shakmaty::CastlingMode::Standard)
            .map_err(|e| Status::invalid_argument(format!("Invalid position: {:?}", e)))?;

        // 2. Intentional blunder logic for low ELO
        if req.elo_rating < 1200 {
            let blunder_chance = calculate_blunder_chance(req.elo_rating);
            
            let mut rng = rand::rng();

            if rng.random_bool(blunder_chance) {
                println!("🎲 Making a random blunder move for ELO {}", req.elo_rating);

                let legal_moves = pos.legal_moves();
                if let Some(random_move) = legal_moves.choose(&mut rng) {
                    

                    let san = San::from_move(&pos, random_move.clone()).to_string();
                    let uci_move_str = random_move.to_uci(shakmaty::CastlingMode::Standard).to_string();


                    let (from, to, piece, captured, promotion) = extract_move_details(&random_move)?;

                    println!("📤 Sending random blunder response: {}", san);

                    return Ok(Response::new(MoveResponse {
                        best_move: uci_move_str,
                        score: -9999, // Fake score indicating a bad/random move
                        from,
                        to,
                        piece,
                        captured,
                        promotion,
                        san,
                    }));
                }
            }
        }

        // 3. Fallback to Stockfish
        let mut stockfish = self.pool.get().await.map_err(|e| {
            eprintln!("❌ Failed to get Stockfish from pool: {}", e);
            Status::internal("Pool exhausted")
        })?;

        println!("✅ Got Stockfish from pool");

        let skill_level = calculate_skill_from_elo(req.elo_rating);
        let depth = calculate_depth_from_elo(req.elo_rating);

        println!("🎯 Skill level: {}, depth: {}", skill_level, depth);

        let fen_str = req.fen.clone();
        let result = tokio::task::spawn_blocking(move || {
            // Skill level setup
            stockfish
                .uci_send(&format!("setoption name Skill Level value {}", skill_level))
                .map_err(|e| format!("Skill setup error: {}", e))?;

            // Position setup
            stockfish
                .set_fen_position(&fen_str)
                .map_err(|e| format!("Invalid FEN: {}", e))?;

            // Calculating best move
            stockfish.set_depth(depth as u32);
            let engine_result = stockfish.go().map_err(|e| format!("Engine error: {}", e))?;

            Ok::<_, String>(engine_result)
        })
        .await
        .map_err(|e| Status::internal(format!("Spawn error: {}", e)))?
        .map_err(|e| Status::internal(e))?;

        let uci_move_str = result.best_move().to_string();
        println!("✅ Got best move from engine: {}", uci_move_str);

        let uci_move: UciMove = uci_move_str
            .parse()
            .map_err(|e| Status::internal(format!("Invalid UCI move from engine: {:?}", e)))?;

        let chess_move = uci_move
            .to_move(&pos)
            .map_err(|e| Status::internal(format!("Illegal move from engine: {:?}", e)))?;

        let (from, to, piece, captured, promotion) = extract_move_details(&chess_move)?;
        
        // Віддаємо клон ходу до San::from_move
        let san = San::from_move(&pos, chess_move.clone()).to_string();

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
