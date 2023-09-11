use std::collections::hash_map::Entry;
use std::{collections::HashMap, sync::Arc};

use egui::{pos2, Color32, Context, Mesh, Rect, Vec2};
use egui_extras::RetainedImage;
use reqwest::header::USER_AGENT;

use crate::mercator::TileId;

use wasm_bindgen_futures::futures_0_3::spawn_local;

#[derive(Clone)]
pub struct Tile {
    image: Arc<RetainedImage>,
}

impl Tile {
    fn from_image_bytes(image: &[u8]) -> Result<Self, String> {
        RetainedImage::from_image_bytes("debug_name", image).map(|image| Self {
            image: Arc::new(image),
        })
    }

    pub fn rect(&self, screen_position: Vec2) -> Rect {
        let tile_size = pos2(self.image.width() as f32, self.image.height() as f32);
        Rect::from_two_pos(
            screen_position.to_pos2(),
            (screen_position + tile_size.to_vec2()).to_pos2(),
        )
    }

    pub fn mesh(&self, screen_position: Vec2, ctx: &Context) -> Mesh {
        let mut mesh = Mesh::with_texture(self.image.texture_id(ctx));
        mesh.add_rect_with_uv(
            self.rect(screen_position),
            Rect::from_min_max(pos2(0., 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        mesh
    }
}

type Source = Box<dyn Fn(TileId) -> String + Send>;

/// Downloads and keeps cache of the tiles. It must persist between frames.
pub struct Tiles {
    cache: HashMap<TileId, Option<Tile>>,
    client: reqwest::Client,
    egui_ctx: Context,
    source: Source,
}

impl Tiles {
    pub fn new<S>(source: S, egui_ctx: Context) -> Self
    where
        S: Fn(TileId) -> String + Send + 'static,
    {
        Self {
            cache: Default::default(),
            client: reqwest::Client::new(),
            egui_ctx: egui_ctx,
            source: Box::new(source),
        }
    }

    /// Return a tile if already in cache, schedule a download otherwise.
    pub fn at(&mut self, tile_id: TileId) -> Option<Tile> {
        match self.cache.entry(tile_id) {
            Entry::Occupied(entry) => entry.get().clone(),
            Entry::Vacant(_entry) => {
                let url = (self.source)(tile_id);
                let cl = self.client.clone();

                let (tx, rx) = std::sync::mpsc::channel::<(TileId, Tile)>();

                match rx.try_recv() {
                    Ok((tile_id, tile)) => {
                        // add it to the cache
                        self.cache.insert(tile_id, Some(tile));

                        // update the gui with new state
                        self.egui_ctx.request_repaint();
                    }
                    Err(e) => {
                        log::warn!("Could not download '{}': {}", &url, e);
                    }
                }

                spawn_local(async move {
                    let _ = download_single(&cl, &url, tile_id, tx).await;
                    log::info!("retrieved tiles");
                });
                None
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Http(reqwest::Error),

    #[error("error while decoding the image: {0}")]
    Image(String),
}

async fn download_single(
    client: &reqwest::Client,
    url: &str,
    tile_id: TileId,
    tx: std::sync::mpsc::Sender<(TileId, Tile)>,
) -> Result<(), Error> {
    let image = client
        .get(url)
        .header(USER_AGENT, "Walkers")
        .send()
        .await
        .map_err(Error::Http)?;

    log::debug!("Downloaded {:?}.", image.status());

    let image = image
        .error_for_status()
        .map_err(Error::Http)?
        .bytes()
        .await
        .map_err(Error::Http)?;

    let res = Tile::from_image_bytes(&image).map_err(Error::Image)?;

    tx.send((tile_id, res)).unwrap();

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    static TILE_ID: TileId = TileId {
        x: 1,
        y: 2,
        zoom: 3,
    };

    type Source = Box<dyn Fn(TileId) -> String + Send>;

    /// Creates `mockito::Server` and function mapping `TileId` to this
    /// server's URL.
    fn mockito_server() -> (mockito::ServerGuard, Source) {
        let server = mockito::Server::new();
        let url = server.url();

        let source = move |tile_id: TileId| {
            format!("{}/{}/{}/{}.png", url, tile_id.zoom, tile_id.x, tile_id.y)
        };

        (server, Box::new(source))
    }

    #[test]
    fn download_single_tile() {
        let _ = env_logger::try_init();

        let (mut server, source) = mockito_server();
        let tile_mock = server
            .mock("GET", "/3/1/2.png")
            .with_body(include_bytes!("valid.png"))
            .create();

        let mut tiles = Tiles::new(source, Context::default());

        // First query start the download, but it will always return None.
        assert!(tiles.at(TILE_ID).is_none());

        // Eventually it gets downloaded and become available in cache.
        while tiles.at(TILE_ID).is_none() {}

        tile_mock.assert();
    }

    fn assert_tile_is_empty_forever(tiles: &mut Tiles) {
        // Should be None now, and forever.
        assert!(tiles.at(TILE_ID).is_none());
        std::thread::sleep(Duration::from_secs(1));
        assert!(tiles.at(TILE_ID).is_none());
    }

    #[test]
    fn tile_is_empty_forever_if_http_returns_error() {
        let _ = env_logger::try_init();

        let (mut server, source) = mockito_server();
        let mut tiles = Tiles::new(source, Context::default());
        let tile_mock = server.mock("GET", "/3/1/2.png").with_status(404).create();

        assert_tile_is_empty_forever(&mut tiles);
        tile_mock.assert();
    }

    #[test]
    fn tile_is_empty_forever_if_http_returns_no_body() {
        let _ = env_logger::try_init();

        let (mut server, source) = mockito_server();
        let mut tiles = Tiles::new(source, Context::default());
        let tile_mock = server.mock("GET", "/3/1/2.png").create();

        assert_tile_is_empty_forever(&mut tiles);
        tile_mock.assert();
    }

    #[test]
    fn tile_is_empty_forever_if_http_returns_garbage() {
        let _ = env_logger::try_init();

        let (mut server, source) = mockito_server();
        let mut tiles = Tiles::new(source, Context::default());
        let tile_mock = server
            .mock("GET", "/3/1/2.png")
            .with_body("definitely not an image")
            .create();

        assert_tile_is_empty_forever(&mut tiles);
        tile_mock.assert();
    }

    #[test]
    fn tile_is_empty_forever_if_http_can_not_even_connect() {
        let _ = env_logger::try_init();

        let source = |_| "totally invalid url".to_string();
        let mut tiles = Tiles::new(source, Context::default());

        assert_tile_is_empty_forever(&mut tiles);
    }
}
