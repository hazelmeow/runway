use rbxcloud::rbx::assets::AssetType;
use thiserror::Error;

use crate::asset::Asset;

mod alpha_bleed;
mod image;

use self::{
    alpha_bleed::alpha_bleed,
    image::{DecodeError, Image},
};

pub fn preprocess(asset: &mut Asset) -> Result<(), PreprocessError> {
    if matches!(asset.ident.asset_type(), AssetType::DecalPng) {
        match Image::decode_png(asset.contents.as_slice()) {
            Ok(mut image) => {
                log::debug!("Preprocessing {}: applying alpha bleed", asset.ident);
                alpha_bleed(&mut image);

                let mut new_contents = Vec::new();
                image.encode_png(&mut new_contents)?;

                asset.contents = debug_ignore::DebugIgnore(new_contents);
            }
            Err(DecodeError::ColorType(png::ColorType::Rgb | png::ColorType::Grayscale)) => {
                // doesn't have transparency
            }
            Err(e) => {
                log::warn!("Preprocessing {}: skipping alpha bleed: {}", asset.ident, e);
            }
        }
    }

    Ok(())
}

#[derive(Debug, Error)]
pub enum PreprocessError {
    #[error(transparent)]
    DecodePng(#[from] png::DecodingError),

    #[error(transparent)]
    EncodePng(#[from] png::EncodingError),
}
