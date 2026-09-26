//! Pictures and the one animation an editor plays.
//!
//! A GIF, animated WebP or APNG is kept as its compressed bytes and its first
//! frame, which is what it shows. It plays only while the pointer rests on it:
//! decoding every frame up front costs its width × height × 4 bytes per frame
//! (a one-megabyte screen recording decodes to hundreds of megabytes), and
//! repainting on every frame keeps the GPU driver's working pools resident.
//!
//! While it plays, a thread decodes frames ahead of the clock. Once a whole
//! pass has fit in [`FRAME_BUDGET`] the frames are kept and replayed; a larger
//! animation keeps only the [`LOOKAHEAD`] frames in flight and is decoded again
//! on every loop.
//!
//! Every frame is its own [`RenderImage`], because the window's sprite atlas
//! keeps what it uploads until the image is dropped from it: a frame the player
//! moves off is [retired](Player::take_retired) so paint can drop it.
use gpui::RenderImage;
use image::{
    AnimationDecoder, Frame, Frames,
    codecs::{gif::GifDecoder, png::PngDecoder, webp::WebPDecoder},
};
use std::{
    cell::RefCell,
    io::Cursor,
    sync::{
        Arc,
        mpsc::{Receiver, SyncSender, TryRecvError, sync_channel},
    },
    time::Duration,
};

/// The decoded frames of one animation kept for replay. The same order as the
/// thresholds browsers keep a whole animation under (20 MB in Firefox, 30 MB in
/// WebKit).
pub(crate) const FRAME_BUDGET: usize = 24 * 1024 * 1024;
/// Frames decoded ahead of the one on screen.
const LOOKAHEAD: usize = 8;
/// How soon to look again when the decoder has not caught up.
const STALL_RETRY: Duration = Duration::from_millis(10);
/// What a frame asking for no delay, or next to none, is shown for. Browsers
/// agree on this: such files were made for players that ignored the delay.
const DEFAULT_DELAY: Duration = Duration::from_millis(100);

/// A decoded image as the editor draws it.
pub(crate) struct Picture {
    poster: Arc<RenderImage>,
    animation: Option<Animation>,
}

/// Two pictures are the same one decoded once, as two images are.
impl PartialEq for Picture {
    fn eq(&self, other: &Self) -> bool {
        self.poster == other.poster
    }
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picture")
            .field("poster", &self.poster)
            .field("animated", &self.is_animated())
            .finish()
    }
}

struct Animation {
    format: AnimatedFormat,
    bytes: Arc<[u8]>,
    first_delay: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnimatedFormat {
    Gif,
    Webp,
    Apng,
}

impl AnimatedFormat {
    /// The animated format `bytes` hold, if they may hold more than one frame.
    /// A GIF is only known to be still once its second frame is looked for.
    pub(crate) fn of(format: gpui::ImageFormat, bytes: &[u8]) -> Option<Self> {
        match format {
            gpui::ImageFormat::Gif => Some(Self::Gif),
            gpui::ImageFormat::Webp => WebPDecoder::new(Cursor::new(bytes))
                .is_ok_and(|decoder| decoder.has_animation())
                .then_some(Self::Webp),
            gpui::ImageFormat::Png => PngDecoder::new(Cursor::new(bytes))
                .and_then(|decoder| decoder.is_apng())
                .unwrap_or(false)
                .then_some(Self::Apng),
            _ => None,
        }
    }

    fn frames(self, bytes: Arc<[u8]>) -> image::ImageResult<Frames<'static>> {
        let bytes = Cursor::new(bytes);
        Ok(match self {
            Self::Gif => GifDecoder::new(bytes)?.into_frames(),
            Self::Webp => WebPDecoder::new(bytes)?.into_frames(),
            Self::Apng => PngDecoder::new(bytes)?.apng()?.into_frames(),
        })
    }
}

impl Picture {
    /// A picture with a single frame.
    pub(crate) fn still(image: Arc<RenderImage>) -> Self {
        Self {
            poster: image,
            animation: None,
        }
    }

    /// Decode the first frame of an animated file and keep its bytes to play
    /// the rest from. `None` when not even the first frame decodes.
    pub(crate) fn animated(format: AnimatedFormat, bytes: Vec<u8>) -> Option<Self> {
        let bytes: Arc<[u8]> = bytes.into();
        let mut frames = format.frames(bytes.clone()).ok()?.filter_map(Result::ok);
        let (poster, first_delay) = render_frame(frames.next()?);
        let animation = frames.next().is_some().then_some(Animation {
            format,
            bytes,
            first_delay,
        });
        Some(Self { poster, animation })
    }

    pub(crate) fn size(&self) -> gpui::Size<gpui::DevicePixels> {
        self.poster.size(0)
    }

    pub(crate) fn is_animated(&self) -> bool {
        self.animation.is_some()
    }
}

/// A decoded frame as the atlas takes it, and how long it is shown.
fn render_frame(frame: Frame) -> (Arc<RenderImage>, Duration) {
    let delay = delay_of(&frame);
    let mut frame = frame;
    // The atlas takes BGRA.
    for pixel in frame.buffer_mut().chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    (Arc::new(RenderImage::new(vec![frame])), delay)
}

fn delay_of(frame: &Frame) -> Duration {
    let (numerator, denominator) = frame.delay().numer_denom_ms();
    let delay = Duration::from_secs_f64(numerator as f64 / denominator.max(1) as f64 / 1000.);
    if delay <= Duration::from_millis(10) {
        DEFAULT_DELAY
    } else {
        delay
    }
}

fn frame_bytes(image: &RenderImage) -> usize {
    image.as_bytes(0).map_or(0, <[u8]>::len)
}

type Decoded = (Arc<RenderImage>, Duration);

/// Decode `animation` pass after pass, sending every frame but the first
/// pass's first, which the receiver already has as the poster. It stops after
/// one pass whose frames fit `budget`, since the receiver kept them,
/// and whenever the receiver is gone.
fn decode(animation: &Animation, budget: usize, frames: SyncSender<Decoded>) {
    let mut first_pass = true;
    loop {
        let Ok(pass) = animation.format.frames(animation.bytes.clone()) else {
            return;
        };
        let mut bytes = 0;
        let mut count = 0;
        for frame in pass.filter_map(Result::ok) {
            let decoded = render_frame(frame);
            bytes += frame_bytes(&decoded.0);
            count += 1;
            if first_pass && count == 1 {
                continue;
            }
            if frames.send(decoded).is_err() {
                return;
            }
        }
        if count < 2 || (first_pass && bytes <= budget) {
            return;
        }
        first_pass = false;
    }
}

struct Playing {
    picture: Arc<Picture>,
    shown: Arc<RenderImage>,
    decoded: Receiver<Decoded>,
    /// The first pass's frames, poster first, for as long as they fit the
    /// budget.
    kept: Option<Vec<Decoded>>,
    kept_bytes: usize,
    /// Where replay stands once the decoder has stopped with every frame kept.
    replaying: Option<usize>,
}

/// The frame on screen of the picture playing, if one is.
#[derive(Clone, Default)]
pub(crate) struct Shown(Option<(Arc<Picture>, Arc<RenderImage>)>);

impl Shown {
    /// What to draw for `picture`: the frame on screen if it is the one
    /// playing, its poster otherwise.
    pub(crate) fn image(&self, picture: &Arc<Picture>) -> Arc<RenderImage> {
        match &self.0 {
            Some((playing, shown)) if Arc::ptr_eq(playing, picture) => shown.clone(),
            _ => picture.poster.clone(),
        }
    }

    pub(crate) fn is_playing(&self, picture: &Arc<Picture>) -> bool {
        self.0
            .as_ref()
            .is_some_and(|(playing, _)| Arc::ptr_eq(playing, picture))
    }

    pub(crate) fn any(&self) -> bool {
        self.0.is_some()
    }
}

/// The animation an editor is playing, if any.
pub(crate) struct Player {
    playing: Option<Playing>,
    /// See [`FRAME_BUDGET`].
    budget: usize,
    /// Frames no longer shown, which paint drops from the atlas.
    retired: RefCell<Vec<Arc<RenderImage>>>,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            playing: None,
            budget: FRAME_BUDGET,
            retired: RefCell::default(),
        }
    }
}

impl Player {
    pub(crate) fn playing(&self) -> Option<&Arc<Picture>> {
        self.playing.as_ref().map(|playing| &playing.picture)
    }

    /// Start `picture` from its first frame and say how long that frame shows.
    /// `None` when it does not animate or no decoder could be started.
    pub(crate) fn play(&mut self, picture: &Arc<Picture>) -> Option<Duration> {
        self.stop();
        let animation = picture.animation.as_ref()?;
        let first_delay = animation.first_delay;
        let (sender, decoded) = sync_channel(LOOKAHEAD);
        let decoding = picture.clone();
        let budget = self.budget;
        std::thread::Builder::new()
            .name("markraft-animation".into())
            .spawn(move || {
                if let Some(animation) = &decoding.animation {
                    decode(animation, budget, sender);
                }
            })
            .ok()?;
        self.playing = Some(Playing {
            picture: picture.clone(),
            shown: picture.poster.clone(),
            decoded,
            kept: Some(vec![(picture.poster.clone(), first_delay)]),
            kept_bytes: frame_bytes(&picture.poster),
            replaying: None,
        });
        Some(first_delay)
    }

    /// Stop playing and go back to the poster. Dropping the receiver ends the
    /// decoder at its next frame.
    pub(crate) fn stop(&mut self) {
        if let Some(playing) = self.playing.take()
            && !Arc::ptr_eq(&playing.shown, &playing.picture.poster)
        {
            self.retired.borrow_mut().push(playing.shown);
        }
    }

    /// Show the next frame if it is ready, and say when to ask again. `None`
    /// once there is nothing more to show: nothing plays, or the decoder gave
    /// out before a pass was kept.
    pub(crate) fn advance(&mut self) -> Option<Duration> {
        let playing = self.playing.as_mut()?;
        let (next, delay) = if let Some(at) = playing.replaying {
            let kept = playing.kept.as_ref()?;
            let at = (at + 1) % kept.len();
            playing.replaying = Some(at);
            kept[at].clone()
        } else {
            match playing.decoded.try_recv() {
                Ok(decoded) => {
                    if let Some(kept) = &mut playing.kept {
                        playing.kept_bytes += frame_bytes(&decoded.0);
                        if playing.kept_bytes <= self.budget {
                            kept.push(decoded.clone());
                        } else {
                            playing.kept = None;
                        }
                    }
                    decoded
                }
                Err(TryRecvError::Empty) => return Some(STALL_RETRY),
                Err(TryRecvError::Disconnected) => {
                    let kept = playing.kept.as_ref().filter(|kept| kept.len() > 1)?;
                    playing.replaying = Some(0);
                    kept[0].clone()
                }
            }
        };
        let previous = std::mem::replace(&mut playing.shown, next);
        if !Arc::ptr_eq(&previous, &playing.shown) {
            self.retired.borrow_mut().push(previous);
        }
        Some(delay)
    }

    /// What paint is to draw, taken before it starts.
    pub(crate) fn shown(&self) -> Shown {
        Shown(
            self.playing
                .as_ref()
                .map(|playing| (playing.picture.clone(), playing.shown.clone())),
        )
    }

    /// The frames moved off since the last call, for paint to drop from the
    /// atlas.
    pub(crate) fn take_retired(&self) -> Vec<Arc<RenderImage>> {
        std::mem::take(&mut *self.retired.borrow_mut())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use image::{Delay, RgbaImage, codecs::gif::GifEncoder};

    /// A GIF of `count` frames, each `side` pixels square and filled with its
    /// own shade, shown for `delay_ms`.
    pub(crate) fn gif(count: u8, side: u32, delay_ms: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            encoder
                .encode_frames((0..count).map(|n| {
                    Frame::from_parts(
                        RgbaImage::from_pixel(side, side, image::Rgba([n * 10, 0, 0, 255])),
                        0,
                        0,
                        Delay::from_numer_denom_ms(delay_ms, 1),
                    )
                }))
                .unwrap();
        }
        bytes
    }

    fn animated(bytes: Vec<u8>) -> Arc<Picture> {
        Arc::new(Picture::animated(AnimatedFormat::Gif, bytes).unwrap())
    }

    fn red(image: &RenderImage) -> u8 {
        // BGRA: red is the third byte.
        image.as_bytes(0).unwrap()[2]
    }

    /// Advance until a frame other than a stall comes, and read its shade.
    fn next_shade(player: &mut Player, picture: &Arc<Picture>) -> u8 {
        let before = player.shown().image(picture);
        for _ in 0..1000 {
            let delay = player.advance().expect("still playing");
            let now = player.shown().image(picture);
            if !Arc::ptr_eq(&before, &now) || delay != STALL_RETRY {
                return red(&now);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the decoder never caught up");
    }

    #[test]
    fn an_animated_gif_keeps_only_its_first_frame_until_it_plays() {
        let picture = animated(gif(5, 4, 50));
        assert!(picture.is_animated());
        assert_eq!(picture.poster.frame_count(), 1);
        assert_eq!(red(&picture.poster), 0);
        assert_eq!(picture.size().width.0, 4);

        let still = Picture::animated(AnimatedFormat::Gif, gif(1, 4, 50)).unwrap();
        assert!(!still.is_animated(), "one frame is a still picture");
    }

    #[test]
    fn a_small_animation_is_decoded_once_and_replayed() {
        let picture = animated(gif(3, 4, 50));
        let mut player = Player::default();
        assert_eq!(player.play(&picture), Some(Duration::from_millis(50)));
        assert!(Arc::ptr_eq(
            &player.shown().image(&picture),
            &picture.poster
        ));
        let shades: Vec<u8> = (0..7).map(|_| next_shade(&mut player, &picture)).collect();
        assert_eq!(shades, [10, 20, 0, 10, 20, 0, 10]);
        let playing = player.playing.as_ref().unwrap();
        assert!(playing.replaying.is_some(), "the kept pass is replayed");
        assert!(
            Arc::ptr_eq(&playing.kept.as_ref().unwrap()[0].0, &picture.poster),
            "the poster opens the kept pass"
        );
        // Every frame moved off waits for paint to drop it from the atlas.
        assert_eq!(player.take_retired().len(), 7);
        assert!(player.take_retired().is_empty());
    }

    #[test]
    fn a_large_animation_keeps_no_pass_and_is_decoded_again_each_loop() {
        // 4² × 4 bytes is 64 bytes a frame: three frames overrun the budget.
        let picture = animated(gif(3, 4, 50));
        let mut player = Player {
            budget: 150,
            ..Player::default()
        };
        player.play(&picture);
        let shades: Vec<u8> = (0..5).map(|_| next_shade(&mut player, &picture)).collect();
        assert_eq!(shades, [10, 20, 0, 10, 20]);
        let playing = player.playing.as_ref().unwrap();
        assert!(playing.kept.is_none(), "nothing past the budget is kept");
        assert!(playing.replaying.is_none());
    }

    #[test]
    fn stopping_goes_back_to_the_poster_and_retires_the_frame_shown() {
        let picture = animated(gif(3, 4, 50));
        let mut player = Player::default();
        player.play(&picture);
        next_shade(&mut player, &picture);
        player.take_retired();
        player.stop();
        assert!(player.playing().is_none());
        assert!(Arc::ptr_eq(
            &player.shown().image(&picture),
            &picture.poster
        ));
        assert_eq!(player.take_retired().len(), 1);
        assert_eq!(player.advance(), None, "nothing plays");
    }

    #[test]
    fn a_still_picture_does_not_play() {
        let still = Arc::new(Picture::animated(AnimatedFormat::Gif, gif(1, 4, 50)).unwrap());
        let mut player = Player::default();
        assert_eq!(player.play(&still), None);
        assert!(player.playing().is_none());
    }

    #[test]
    fn a_frame_asking_for_no_delay_is_shown_for_the_default() {
        let picture = animated(gif(2, 4, 0));
        assert_eq!(Player::default().play(&picture), Some(DEFAULT_DELAY));
    }
}
