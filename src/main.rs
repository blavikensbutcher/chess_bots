use stockfish::Stockfish;
use tonic::{transport::Server, Request, Response, Status};
use shakmaty::{Chess, uci::UciMove, san::San};
use shakmaty::fen::Fen;
use dotenv::dotenv;
mod config;
use std::net::SocketAddr;
use tokio::time::{timeout, Duration};


pub mod chess_bot {
    tonic::include_proto!("chess_bot");
}


use chess_bot::chess_bot_server::{ChessBot, ChessBotServer};
use chess_bot::{MoveResponse, PositionRequest};


use config::Config;


#[derive(Debug, Default)]
pub struct ChessBotService;


#[tonic::async_trait]
impl ChessBot for ChessBotService {
    async fn get_best_move(
        &self,
        request: Request<PositionRequest>,
    ) -> Result<Response<MoveResponse>, Status> {
        let req = request.into_inner();
        
        println!("📥 Received request: FEN={}, ELO={}", &req.fen[..30], req.elo_rating);
        
        let stockfish_path =
            std::env::var("STOCKFISH_PATH").unwrap_or_else(|_| "/usr/games/stockfish".to_string());


        println!("🔧 Starting Stockfish at {}", stockfish_path);


        let mut stockfish = Stockfish::new(&stockfish_path)
            .map_err(|e| {
                eprintln!("❌ Failed to start Stockfish: {}", e);
                Status::internal(format!("Failed to start Stockfish: {}", e))
            })?;


        println!("✅ Stockfish started");


        stockfish.setup_for_new_game()
            .map_err(|e| Status::internal(format!("Setup error: {}", e)))?;


        let skill_level = calculate_skill_from_elo(req.elo_rating);
        println!("🎯 Skill level: {}", skill_level);
        
        stockfish.uci_send(&format!("setoption name Skill Level value {}", skill_level))
            .map_err(|e| Status::internal(format!("Skill setup error: {}", e)))?;
        
        stockfish.uci_send("setoption name MultiPV value 1")
            .map_err(|e| Status::internal(format!("MultiPV error: {}", e)))?;


        stockfish.set_fen_position(&req.fen)
            .map_err(|e| Status::invalid_argument(format!("Invalid FEN: {}", e)))?;


        println!("🔍 Starting analysis...");


        let depth = calculate_depth_from_elo(req.elo_rating);
        println!("🎯 Using depth: {}", depth);


        let result = timeout(Duration::from_secs(3), async {
            tokio::task::spawn_blocking(move || {
                stockfish.set_depth(depth as u32);
                stockfish.go()
            }).await
        })
        .await
        .map_err(|_| {
            eprintln!("❌ Stockfish timeout after 3s");
            Status::deadline_exceeded("Stockfish analysis timed out")
        })?
        .map_err(|e| Status::internal(format!("Spawn error: {}", e)))?
        .map_err(|e| {
            eprintln!("❌ Engine error: {}", e);
            Status::internal(format!("Engine error: {}", e))
        })?;


        println!("✅ Got best move: {}", result.best_move());


        let uci_move_str = result.best_move().to_string();
        
        let fen: Fen = req.fen.parse()
            .map_err(|e| Status::invalid_argument(format!("Invalid FEN: {:?}", e)))?;
        
        let pos: Chess = fen.into_position(shakmaty::CastlingMode::Standard)
            .map_err(|e| Status::invalid_argument(format!("Invalid position: {:?}", e)))?;


        let uci_move: UciMove = uci_move_str.parse()
            .map_err(|e| Status::internal(format!("Invalid UCI move: {:?}", e)))?;
        
        let chess_move = uci_move.to_move(&pos)
            .map_err(|e| Status::internal(format!("Illegal move: {:?}", e)))?;


        let (from, to, piece, captured, promotion) = match &chess_move {
            shakmaty::Move::Normal { role, from, to, capture, promotion } => {
                let piece_name = format!("{:?}", role);
                let captured_name = capture.map(|c| format!("{:?}", c));
                let promotion_name = promotion.map(|p| format!("{:?}", p));
                
                (
                    from.to_string(),
                    to.to_string(),
                    piece_name,
                    captured_name,
                    promotion_name
                )
            },
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
                    None
                )
            },
            shakmaty::Move::EnPassant { from, to } => {
                (
                    from.to_string(),
                    to.to_string(),
                    "Pawn".to_string(),
                    Some("Pawn".to_string()),
                    None
                )
            },
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
        ..=800 => 0,
        801..=1000 => 3,
        1001..=1200 => 6,
        1201..=1400 => 9,
        1401..=1600 => 12,
        1601..=1800 => 15,
        1801..=2000 => 18,
        _ => 20,
    }
}


fn calculate_depth_from_elo(elo: i32) -> u8 {
    match elo {
        ..=800 => 3,
        801..=1000 => 5,
        1001..=1200 => 7,
        1201..=1400 => 9,
        1401..=1600 => 11,
        1601..=1800 => 13,
        1801..=2000 => 15,
        _ => 17,
    }
}


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();
    let config = Config::from_env().expect("Failed to load configuration");
    let host = &config.server_host;
    let port: u16 = config.server_port;
    let addr = SocketAddr::new(host.parse()?, port);
    let bot_service = ChessBotService::default();


    println!("Chess Bot gRPC Server listening on {}", config.server_address());


    Server::builder()
        .add_service(ChessBotServer::new(bot_service))
        .serve(addr)
        .await?;


    Ok(())
}
