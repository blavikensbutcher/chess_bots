use deadpool::managed::Pool;
use dotenv::dotenv;
use shakmaty::fen::Fen;
use shakmaty::{san::San, uci::UciMove, Chess};
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
) -> Result<(String, String, String, Option<String>, Option<String>), Status> {
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
        shakmaty::Move::Put { .. } => Err(Status::internal("Put move not supported")),
    }
}

pub fn calculate_skill_from_elo(elo: i32) -> i32 {
    match elo {
        ..=227  => -20,
        ..=283  => -19,
        ..=339  => -18,
        ..=395  => -17,
        ..=451  => -16,
        ..=507  => -15,
        ..=563  => -14,
        ..=619  => -13,
        ..=675  => -12,
        ..=731  => -11,
        ..=787  => -10,
        ..=843  => -9,
        ..=899  => -8,
        ..=955  => -7,
        ..=1011 => -6,
        ..=1067 => -5,
        ..=1123 => -4,
        ..=1179 => -3,
        ..=1235 => -2,
        ..=1291 => -1,
        ..=1347 =>  0,
        ..=1403 =>  1,
        ..=1459 =>  2,
        ..=1515 =>  3,
        ..=1571 =>  4,
        ..=1627 =>  5,
        ..=1683 =>  6,
        ..=1739 =>  7,
        ..=1795 =>  8,
        ..=1851 =>  9,
        ..=1907 => 10,
        ..=1963 => 11,
        ..=2019 => 12,
        ..=2075 => 13,
        ..=2131 => 14,
        ..=2187 => 15,
        ..=2243 => 16,
        ..=2299 => 17,
        ..=2355 => 18,
        ..=2411 => 19,
        _       => 20,
    }
}

fn calculate_depth_from_elo(elo: i32) -> u8 {
    match elo {
        ..=1999 => 5,
        2000..=2299 => 8,
        2300..=2599 => 13,
        _ => 22,
    }
}

fn calculate_multipv_from_elo(elo: i32) -> u8 {
    match elo {
        ..=2599 => 4,
        _ => 1,
    }
}

#[tonic::async_trait]
impl ChessBot for ChessBotService {
    async fn get_best_move(
        &self,
        request: Request<PositionRequest>,
    ) -> Result<Response<MoveResponse>, Status> {
        let req = request.into_inner();

        let fen_preview: String = req.fen.chars().take(30).collect();
        println!(
            "📥 Received request: FEN={}, ELO={}",
            fen_preview,
            req.elo_rating
        );

        let mut stockfish = self.pool.get().await.map_err(|e| {
            eprintln!("❌ Failed to get Stockfish from pool: {}", e);
            Status::internal("Pool exhausted")
        })?;

        println!("✅ Got Stockfish from pool");

        let skill_level = calculate_skill_from_elo(req.elo_rating);
        let depth = calculate_depth_from_elo(req.elo_rating);
        let multipv = calculate_multipv_from_elo(req.elo_rating);

        println!(
            "🎯 Skill level: {}, depth: {}, multipv: {}",
            skill_level, depth, multipv
        );

        let fen_str = req.fen.clone();
        let result = tokio::task::spawn_blocking(move || {
            stockfish
                .uci_send("setoption name UCI_Variant value chess")
                .map_err(|e| format!("Variant setup error: {}", e))?;

            stockfish
                .uci_send(&format!("setoption name MultiPV value {}", multipv))
                .map_err(|e| format!("MultiPV setup error: {}", e))?;

            stockfish
                .uci_send(&format!("setoption name Skill Level value {}", skill_level))
                .map_err(|e| format!("Skill setup error: {}", e))?;

            stockfish
                .set_fen_position(&fen_str)
                .map_err(|e| format!("Invalid FEN: {}", e))?;

            stockfish.set_depth(depth as u32);

            let engine_result = stockfish
                .go()
                .map_err(|e| format!("Engine error: {}", e))?;

            Ok::<_, String>(engine_result)
        })
        .await
        .map_err(|e| Status::internal(format!("Spawn error: {}", e)))?
        .map_err(|e| Status::internal(e))?;

        let uci_move_str = result.best_move().to_string();
        println!("✅ Got best move from engine: {}", uci_move_str);

        let fen: Fen = req
            .fen
            .parse()
            .map_err(|e| Status::invalid_argument(format!("Invalid FEN: {:?}", e)))?;

        let pos: Chess = fen
            .into_position(shakmaty::CastlingMode::Standard)
            .map_err(|e| Status::invalid_argument(format!("Invalid position {:?}", e)))?;

        let uci_move: UciMove = uci_move_str
            .parse()
            .map_err(|e| Status::internal(format!("Invalid UCI move: {:?}", e)))?;

        let chess_move = uci_move
            .to_move(&pos)
            .map_err(|e| Status::internal(format!("Illegal move: {:?}", e)))?;

        let (from, to, piece, captured, promotion) = extract_move_details(&chess_move)?;
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

    let stockfish_path = std::env::var("STOCKFISH_PATH")
        .unwrap_or_else(|_| "/usr/games/fairy-stockfish".to_string());

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

