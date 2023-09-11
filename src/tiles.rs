use std::collections::hash_map::Entry;
use std::sync::mpsc::Receiver;
use std::{collections::HashMap, sync::Arc};

use egui::{pos2, Color32, Context, Mesh, Rect, Vec2};
use egui_extras::RetainedImage;

use crate::mercator::TileId;

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
    egui_ctx: Context,
    source: Source,
    tx: std::sync::mpsc::Sender<(TileId, Tile)>,
    rx: std::sync::mpsc::Receiver<(TileId, Tile)>,
}

impl Tiles {
    pub fn new<S>(source: S, egui_ctx: Context) -> Self
    where
        S: Fn(TileId) -> String + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel::<(TileId, Tile)>();

        Self {
            cache: Default::default(),
            egui_ctx: egui_ctx,
            source: Box::new(source),
            tx,
            rx,
        }
    }

    /// Return a tile if already in cache, schedule a download otherwise.
    pub fn at(&mut self, tile_id: TileId) -> Option<Tile> {
        match self.cache.entry(tile_id) {
            Entry::Occupied(entry) => entry.get().clone(),
            Entry::Vacant(_entry) => {
                let url = (self.source)(tile_id);

                download_single(&url, tile_id, self.tx.clone()).unwrap();

                match self.rx.try_recv() {
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

                None
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("error while decoding the image: {0}")]
    Image(String),
}

fn download_single(
    url: &str,
    tile_id: TileId,
    tx: std::sync::mpsc::Sender<(TileId, Tile)>,
) -> Result<(), Error> {
    let request = ehttp::Request::get(url);

    ehttp::fetch(request, move |result: ehttp::Result<ehttp::Response>| {
        let result = result.unwrap();
        println!("tile received with code: {:?}", result.status);

        let image = result.bytes;
        let res = Tile::from_image_bytes(&image)
            .map_err(Error::Image)
            .unwrap();

        tx.send((tile_id, res)).unwrap();
    });

    Ok(())
}
