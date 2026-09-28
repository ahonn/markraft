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
//! moves off is [retired](Player::take_retired) so paint can drop it. Kept
//! frames stay uploaded while they are replayed and are retired together when
//! playback stops.
//!
//! Playback also stops once the frames it moves to go unpainted for
//! [`UNPAINTED_LIMIT`]: the editor is hidden or no longer shown, and nothing
//! else would tell the player so.
use gpui::RenderImage;
use image::{
    AnimationDecoder, Frame, Frames,
    codecs::{gif::GifDecoder, png::PngDecoder, webp::WebPDecoder},
    metadata::LoopCount,
};
use std::{
    cell::{Cell, RefCell},
    io::Cursor,
    num::NonZeroU32,
    sync::{
        Arc,
        mpsc::{Receiver, SyncSender, TryRecvError, sync_channel},
    },
    time::{Duration, Instant},
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
/// How long the frames playback moves to may go unpainted before it stops.
const UNPAINTED_LIMIT: Duration = Duration::from_millis(500);

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
    /// How many passes the file asks for; `None` loops while the pointer rests.
    plays: Option<NonZeroU32>,
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

    /// The file's frames, and how many passes it asks for.
    fn frames(self, bytes: Arc<[u8]>) -> image::ImageResult<(Frames<'static>, Option<NonZeroU32>)> {
        fn finite(count: LoopCount) -> Option<NonZeroU32> {
            match count {
                LoopCount::Infinite => None,
                LoopCount::Finite(count) => Some(count),
            }
        }
        let bytes = Cursor::new(bytes);
        Ok(match self {
            Self::Gif => {
                let decoder = GifDecoder::new(bytes)?;
                // A GIF's count is of repeats after the first pass.
                let plays = finite(decoder.loop_count()).and_then(|count| count.checked_add(1));
                (decoder.into_frames(), plays)
            }
            Self::Webp => {
                let decoder = WebPDecoder::new(bytes)?;
                let plays = finite(decoder.loop_count());
                (decoder.into_frames(), plays)
            }
            Self::Apng => {
                let decoder = PngDecoder::new(bytes)?.apng()?;
                let plays = finite(decoder.loop_count());
                (decoder.into_frames(), plays)
            }
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
        let (frames, plays) = format.frames(bytes.clone()).ok()?;
        // A frame that fails to decode ends the animation: what the decoder
        // does after an error is not something to loop on.
        let mut frames = frames.map_while(Result::ok);
        let (poster, first_delay) = render_frame(frames.next()?);
        let animation = frames.next().is_some().then_some(Animation {
            format,
            bytes,
            first_delay,
            plays,
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

type Kept = (Arc<RenderImage>, Duration);

/// What the decoding thread sends the player.
enum Decoded {
    Frame(Arc<RenderImage>, Duration),
    /// The last pass the file asks for has been sent.
    Finished,
}

/// Decode `animation` pass after pass, sending every frame but the first
/// pass's first, which the receiver already has as the poster. It stops after
/// one pass whose frames fit `budget`, since the receiver kept them and counts
/// the passes itself, after the passes the file asks for, and whenever the
/// receiver is gone.
fn decode(animation: &Animation, budget: usize, frames: SyncSender<Decoded>) {
    let mut passes = 0;
    loop {
        let Ok((pass, _)) = animation.format.frames(animation.bytes.clone()) else {
            return;
        };
        let mut bytes = 0;
        let mut count = 0;
        for frame in pass.map_while(Result::ok) {
            let (image, delay) = render_frame(frame);
            bytes += frame_bytes(&image);
            count += 1;
            if passes == 0 && count == 1 {
                continue;
            }
            if frames.send(Decoded::Frame(image, delay)).is_err() {
                return;
            }
        }
        passes += 1;
        if count < 2 || (passes == 1 && bytes <= budget) {
            return;
        }
        if animation.plays.is_some_and(|plays| passes >= plays.get()) {
            let _ = frames.send(Decoded::Finished);
            return;
        }
    }
}

struct Playing {
    picture: Arc<Picture>,
    shown: Arc<RenderImage>,
    decoded: Receiver<Decoded>,
    /// The first pass's frames, poster first, for as long as they fit the
    /// budget.
    kept: Option<Vec<Kept>>,
    kept_bytes: usize,
    /// Where replay stands once the decoder has stopped with every frame kept.
    replaying: Option<usize>,
    /// Whole passes shown of a kept animation, which the player replays and
    /// so counts itself.
    passes: u32,
}

impl Playing {
    /// Whether the file's passes have all been shown.
    fn done(&self) -> bool {
        self.picture
            .animation
            .as_ref()
            .and_then(|animation| animation.plays)
            .is_some_and(|plays| self.passes >= plays.get())
    }

    fn keeps(&self, image: &Arc<RenderImage>) -> bool {
        self.kept
            .as_ref()
            .is_some_and(|kept| kept.iter().any(|(frame, _)| Arc::ptr_eq(frame, image)))
    }
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
    /// See [`UNPAINTED_LIMIT`].
    unpainted_limit: Duration,
    /// Since when the frame on screen has changed without paint taking it.
    unpainted_since: Cell<Option<Instant>>,
    /// Frames no longer shown, which paint drops from the atlas.
    retired: RefCell<Vec<Arc<RenderImage>>>,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            playing: None,
            budget: FRAME_BUDGET,
            unpainted_limit: UNPAINTED_LIMIT,
            unpainted_since: Cell::new(None),
            retired: RefCell::default(),
        }
    }
}

/// One step of playback: when to step again, and whether the frame on screen
/// changed, which is when the editor has to paint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tick {
    pub(crate) delay: Duration,
    pub(crate) changed: bool,
}

/// Queue `image` for paint to drop from the atlas, unless it is `poster`,
/// which the picture keeps drawing, or already queued.
fn retire(
    retired: &RefCell<Vec<Arc<RenderImage>>>,
    poster: &Arc<RenderImage>,
    image: Arc<RenderImage>,
) {
    let mut retired = retired.borrow_mut();
    if !Arc::ptr_eq(&image, poster) && !retired.iter().any(|queued| Arc::ptr_eq(queued, &image)) {
        retired.push(image);
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
            passes: 0,
        });
        Some(first_delay)
    }

    /// Stop playing and go back to the poster. Dropping the receiver ends the
    /// decoder at its next frame.
    pub(crate) fn stop(&mut self) {
        self.unpainted_since.set(None);
        let Some(playing) = self.playing.take() else {
            return;
        };
        let poster = &playing.picture.poster;
        retire(&self.retired, poster, playing.shown);
        for (frame, _) in playing.kept.into_iter().flatten() {
            retire(&self.retired, poster, frame);
        }
    }

    /// Show the next frame if it is ready, and say when to ask again. `None`
    /// once there is nothing more to show: nothing plays, the file's passes
    /// are over, which leaves its last frame on screen, the decoder gave out
    /// before a pass was kept, which goes back to the poster, or paint has
    /// stopped taking frames, which stops playback.
    pub(crate) fn advance(&mut self) -> Option<Tick> {
        if self
            .unpainted_since
            .get()
            .is_some_and(|since| since.elapsed() >= self.unpainted_limit)
        {
            self.stop();
            return None;
        }
        let playing = self.playing.as_mut()?;
        let poster = playing.picture.poster.clone();
        let mut ended = false;
        let (next, delay) = if let Some(at) = playing.replaying {
            let kept = playing.kept.as_ref()?;
            let mut at = at + 1;
            if at == kept.len() {
                playing.passes += 1;
                if playing.done() {
                    return None;
                }
                at = 0;
            }
            playing.replaying = Some(at);
            playing.kept.as_ref()?[at].clone()
        } else {
            match playing.decoded.try_recv() {
                Ok(Decoded::Frame(image, delay)) => {
                    if let Some(kept) = &mut playing.kept {
                        playing.kept_bytes += frame_bytes(&image);
                        if playing.kept_bytes <= self.budget {
                            kept.push((image.clone(), delay));
                        } else if let Some(kept) = playing.kept.take() {
                            // Too large to keep: what was kept goes, but the
                            // frame on screen only once it is replaced.
                            for (frame, _) in kept {
                                if !Arc::ptr_eq(&frame, &playing.shown) {
                                    retire(&self.retired, &poster, frame);
                                }
                            }
                        }
                    }
                    (image, delay)
                }
                Ok(Decoded::Finished) => return None,
                Err(TryRecvError::Empty) => {
                    return Some(Tick {
                        delay: STALL_RETRY,
                        changed: false,
                    });
                }
                Err(TryRecvError::Disconnected) => {
                    match playing.kept.as_ref().filter(|kept| kept.len() > 1) {
                        Some(kept) => {
                            let first = kept[0].clone();
                            playing.passes += 1;
                            if playing.done() {
                                return None;
                            }
                            playing.replaying = Some(0);
                            first
                        }
                        None => {
                            // The file stopped decoding partway: show the
                            // poster rather than a frame from the middle.
                            ended = true;
                            (poster.clone(), Duration::ZERO)
                        }
                    }
                }
            }
        };
        let previous = std::mem::replace(&mut playing.shown, next);
        if Arc::ptr_eq(&previous, &playing.shown) {
            return (!ended).then_some(Tick {
                delay,
                changed: false,
            });
        }
        if !playing.keeps(&previous) {
            retire(&self.retired, &poster, previous);
        }
        if self.unpainted_since.get().is_none() {
            self.unpainted_since.set(Some(Instant::now()));
        }
        (!ended).then_some(Tick {
            delay,
            changed: true,
        })
    }

    /// What paint is to draw, taken before it starts. Taking it is what tells
    /// the player its frames are still being painted.
    pub(crate) fn shown(&self) -> Shown {
        self.unpainted_since.set(None);
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
    use image::{
        Delay, RgbaImage,
        codecs::gif::{GifEncoder, Repeat},
    };

    /// A GIF of `count` frames, each `side` pixels square and filled with its
    /// own shade, shown for `delay_ms`.
    pub(crate) fn gif(count: u8, side: u32, delay_ms: u32) -> Vec<u8> {
        gif_repeating(count, side, delay_ms, Repeat::Infinite)
    }

    fn gif_repeating(count: u8, side: u32, delay_ms: u32, repeat: Repeat) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            encoder.set_repeat(repeat).unwrap();
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
            let tick = player.advance().expect("still playing");
            let now = player.shown().image(picture);
            if !Arc::ptr_eq(&before, &now) || tick.changed {
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
        // Kept frames stay uploaded while they are replayed, so a loop never
        // uploads a frame the atlas already holds.
        assert!(player.take_retired().is_empty());
        player.stop();
        assert_eq!(
            player.take_retired().len(),
            2,
            "every kept frame but the poster"
        );
    }

    #[test]
    fn a_large_animation_retires_each_frame_it_moves_off() {
        let picture = animated(gif(4, 4, 50));
        let mut player = Player {
            budget: 150,
            ..Player::default()
        };
        player.play(&picture);
        for _ in 0..6 {
            next_shade(&mut player, &picture);
        }
        // Six frames moved to; the first move left the poster, which stays.
        let retired = player.take_retired();
        assert_eq!(retired.len(), 5);
        assert!(
            retired
                .iter()
                .all(|frame| !Arc::ptr_eq(frame, &picture.poster))
        );
    }

    #[test]
    fn a_file_asking_for_passes_stops_on_its_last_frame() {
        // Two repeats after the first pass: three passes of three frames.
        for budget in [FRAME_BUDGET, 150] {
            let picture = animated(gif_repeating(3, 4, 50, Repeat::Finite(2)));
            let mut player = Player {
                budget,
                ..Player::default()
            };
            player.play(&picture);
            let mut shades = Vec::new();
            loop {
                match player.advance() {
                    Some(tick) if tick.changed => shades.push(red(&player.shown().image(&picture))),
                    Some(_) => std::thread::sleep(Duration::from_millis(1)),
                    None => break,
                }
            }
            assert_eq!(shades, [10, 20, 0, 10, 20, 0, 10, 20], "budget {budget}");
            assert_eq!(
                red(&player.shown().image(&picture)),
                20,
                "the last frame stays"
            );
            assert!(player.playing().is_some(), "the pointer still rests on it");
        }
    }

    #[test]
    fn frames_nobody_paints_stop_playback() {
        let picture = animated(gif(3, 4, 50));
        let mut player = Player {
            unpainted_limit: Duration::from_millis(20),
            ..Player::default()
        };
        player.play(&picture);
        next_shade(&mut player, &picture);
        // Paint takes the frame on screen, which keeps playback going.
        player.shown();
        std::thread::sleep(Duration::from_millis(30));
        next_shade(&mut player, &picture);
        assert!(player.playing().is_some());
        // Move on to a frame nothing takes.
        while !player.advance().expect("still playing").changed {
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(player.advance(), None);
        assert!(player.playing().is_none());
        assert!(!player.take_retired().is_empty());
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
