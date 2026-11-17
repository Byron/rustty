//! Cached local worktree discovery and theme-derived panel tints.
use egui::ecolor::{Hsva, gamma_u8_from_linear_f32, linear_f32_from_gamma_u8};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(super) struct State {
    cwd: Option<PathBuf>,
    repository: Option<(PathBuf, u64)>,
    color: Option<(Colors, [u8; 3])>,
}

impl State {
    pub fn update(&mut self, directory: Option<&Path>) {
        if self.cwd.as_deref() == directory {
            return;
        }
        self.cwd = directory.map(Path::to_path_buf);
        let directory = directory
            .filter(|directory| directory.is_absolute())
            .and_then(|directory| directory.canonicalize().ok());
        if directory.as_ref().is_some_and(|directory| {
            self.repository
                .as_ref()
                .is_some_and(|(worktree, _)| directory.starts_with(worktree))
        }) {
            // shortcut: nested repositories keep the outer tint, rediscover if separate nested colors are requested.
            return;
        }
        self.repository = directory.and_then(|directory| {
            let (repository, _) = gix_discover::upwards_opts(
                &directory,
                gix_discover::upwards::Options {
                    dot_git_only: true,
                    ..Default::default()
                },
            )
            .ok()?;
            let (git_dir, worktree) = repository.into_repository_and_work_tree_directories();
            let git_dir = git_dir.canonicalize().ok()?;
            let worktree = worktree?.canonicalize().ok()?;
            Some((
                worktree,
                stable_hash(git_dir.as_os_str().as_encoded_bytes()),
            ))
        });
        self.color = None;
    }

    pub fn background(
        &mut self,
        foreground: [u8; 3],
        background: [u8; 3],
        palette: &[[u8; 3]],
    ) -> [u8; 3] {
        let Some((_, seed)) = self.repository else {
            return background;
        };
        let colors = Colors {
            foreground,
            background,
            palette: std::array::from_fn(|i| palette[i]),
        };
        if let Some((previous, color)) = &self.color
            && previous == &colors
        {
            return *color;
        }
        let color = tint(seed, &colors);
        self.color = Some((colors, color));
        color
    }
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let hash = bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    // Avalanche nearby directory names before splitting the seed into color choices.
    let hash = (hash ^ (hash >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    let hash = (hash ^ (hash >> 27)).wrapping_mul(0x94d049bb133111eb);
    hash ^ (hash >> 31)
}

#[derive(PartialEq, Eq)]
struct Colors {
    foreground: [u8; 3],
    background: [u8; 3],
    palette: [[u8; 3]; 16],
}

fn tint(seed: u64, colors: &Colors) -> [u8; 3] {
    const ACCENTS: [usize; 6] = [1, 3, 2, 6, 4, 5];
    let position = (seed & 0xffff) as f32 / 65536.0 * ACCENTS.len() as f32;
    let brightness = ((seed >> 16) & 0xff) as f32 / 255.0;
    let accent = |index: usize| {
        let index = ACCENTS[index % ACCENTS.len()];
        mix(
            linear(colors.palette[index]),
            linear(colors.palette[index + 8]),
            brightness,
        )
    };
    let mut accent = mix(
        accent(position as usize),
        accent(position as usize + 1),
        position.fract(),
    );
    if accent.iter().copied().fold(f32::NEG_INFINITY, f32::max)
        - accent.iter().copied().fold(f32::INFINITY, f32::min)
        < 0.01
    {
        accent = Hsva::new(position / ACCENTS.len() as f32, 0.65, 1.0, 1.0).to_rgb();
    }
    let background = linear(colors.background);
    let base_luminance = luminance(background);
    let foreground = luminance(linear(colors.foreground));
    let minimum_contrast = contrast(foreground, base_luminance).min(4.5);
    let palette = colors.palette.map(|color| luminance(linear(color)));
    let preserves_contrast = |light| {
        contrast(foreground, light) >= minimum_contrast
            && palette.iter().all(|&foreground| {
                contrast(foreground, base_luminance) < 4.5 || contrast(foreground, light) >= 4.5
            })
    };
    let strength = 0.35 + ((seed >> 24) & 0xff) as f32 / 255.0 * 0.30;
    let target = base_luminance.clamp(0.012, 0.88);
    let accent = with_luminance(
        accent,
        if preserves_contrast(base_luminance + (target - base_luminance) * strength * 0.25) {
            target
        } else {
            base_luminance
        },
    );
    for scale in [1.0, 0.75, 0.5, 0.25] {
        let color = mix(background, accent, strength * scale).map(gamma_u8_from_linear_f32);
        let light = luminance(linear(color));
        if preserves_contrast(light) {
            return color;
        }
        // Correct eight-bit rounding before reducing the tint's strength.
        for channel in [2, 0, 1] {
            for delta in [-1, 1] {
                let mut corrected = color;
                corrected[channel] = corrected[channel].saturating_add_signed(delta);
                if preserves_contrast(luminance(linear(corrected))) {
                    return corrected;
                }
            }
        }
    }
    colors.background
}

fn linear(color: [u8; 3]) -> [f32; 3] {
    color.map(linear_f32_from_gamma_u8)
}

fn mix(a: [f32; 3], b: [f32; 3], amount: f32) -> [f32; 3] {
    std::array::from_fn(|i| a[i] * (1.0 - amount) + b[i] * amount)
}

fn luminance([r, g, b]: [f32; 3]) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn contrast(a: f32, b: f32) -> f32 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn with_luminance(color: [f32; 3], target: f32) -> [f32; 3] {
    let peak = color.into_iter().fold(0.0, f32::max);
    let color = if peak > 0.0 {
        color.map(|channel| channel / peak)
    } else {
        color
    };
    let light = luminance(color);
    if light > target {
        color.map(|channel| channel * target / light)
    } else if light < 1.0 {
        mix(color, [1.0; 3], (target - light) / (1.0 - light))
    } else {
        color
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "rustty-git-background-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        fn repository(&self, worktree: &Path) -> PathBuf {
            let git_dir = worktree.join(".git");
            std::fs::create_dir_all(git_dir.join("objects")).unwrap();
            std::fs::create_dir_all(git_dir.join("refs")).unwrap();
            std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
            git_dir
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn colors() -> Colors {
        let config = rustty::config::Config::default();
        Colors {
            foreground: config.foreground.to_array(),
            background: config.background.to_array(),
            palette: std::array::from_fn(|i| config.palette[i].to_array()),
        }
    }

    #[test]
    fn discovery_caches_worktrees_and_misses_until_the_directory_changes() {
        let fixture = Fixture::new();
        let worktree = fixture.0.join("repo");
        let child = worktree.join("src");
        std::fs::create_dir_all(&child).unwrap();
        let mut state = State::default();
        state.update(None);
        state.update(Some(&worktree));
        assert!(state.repository.is_none());
        let git_dir = fixture.repository(&worktree);
        state.update(Some(&worktree));
        assert!(state.repository.is_none(), "unchanged misses remain cached");
        state.update(Some(&child));
        let repository = state.repository.clone().unwrap();
        assert_eq!(repository.0, worktree);
        assert_eq!(
            repository.1,
            stable_hash(git_dir.as_os_str().as_encoded_bytes())
        );
        std::fs::remove_dir_all(git_dir).unwrap();
        state.update(Some(&worktree));
        assert_eq!(state.repository, Some(repository));
        let neighbor = fixture.0.join("repo-other");
        std::fs::create_dir_all(&neighbor).unwrap();
        state.update(Some(&neighbor));
        assert!(
            state.repository.is_none(),
            "containment uses path components"
        );
        let colors = colors();
        assert_eq!(
            state.background(colors.foreground, colors.background, &colors.palette),
            colors.background
        );
        fixture.repository(&worktree);
        state.update(Some(&worktree));
        assert!(state.repository.is_some());
        state.update(None);
        assert!(state.repository.is_none());
        state.update(Some(&worktree));
        assert!(
            state.repository.is_some(),
            "enabling discovers the current CWD"
        );
    }

    #[test]
    fn linked_worktrees_use_their_private_git_directory() {
        let fixture = Fixture::new();
        let main = fixture.0.join("main");
        let common = fixture.repository(&main);
        let linked = fixture.0.join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        let private = common.join("worktrees/linked");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(private.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(private.join("commondir"), "../..\n").unwrap();
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", private.display()),
        )
        .unwrap();
        let mut state = State::default();
        state.update(Some(&main));
        let main_seed = state.repository.as_ref().unwrap().1;
        state.update(Some(&linked));
        assert_eq!(state.repository.as_ref().unwrap().0, linked);
        assert_eq!(
            state.repository.as_ref().unwrap().1,
            stable_hash(private.as_os_str().as_encoded_bytes())
        );
        assert_ne!(state.repository.as_ref().unwrap().1, main_seed);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_share_identity_and_can_leave_a_cached_worktree() {
        let fixture = Fixture::new();
        let worktree = fixture.0.join("repo");
        fixture.repository(&worktree);
        let alias = fixture.0.join("alias");
        std::os::unix::fs::symlink(&worktree, &alias).unwrap();
        let mut state = State::default();
        state.update(Some(&alias));
        let repository = state.repository.clone();
        state.update(Some(&worktree));
        assert_eq!(state.repository, repository);
        let outside = fixture.0.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let escape = worktree.join("escape");
        std::os::unix::fs::symlink(&outside, &escape).unwrap();
        state.update(Some(&escape));
        assert!(state.repository.is_none());
    }

    #[test]
    fn tints_are_diverse_and_preserve_theme_luminance_and_readable_text() {
        for (background, foreground) in [
            ([40, 44, 52], [255; 3]),
            ([235; 3], [24; 3]),
            ([253, 246, 227], [101, 123, 131]),
            ([0; 3], [255; 3]),
            ([255; 3], [0; 3]),
            ([90, 80, 70], [128; 3]),
        ] {
            let mut colors = colors();
            colors.background = background;
            colors.foreground = foreground;
            let base = luminance(linear(background));
            let text = luminance(linear(foreground));
            let mut unique = std::collections::HashSet::new();
            for index in 0..1024 {
                let seed = stable_hash(format!("/git/project-{index}/.git").as_bytes());
                let color = tint(seed, &colors);
                unique.insert(color);
                let light = luminance(linear(color));
                assert!(contrast(text, light) >= contrast(text, base).min(4.5));
                for foreground in colors.palette {
                    let foreground = luminance(linear(foreground));
                    if contrast(foreground, base) >= 4.5 {
                        assert!(contrast(foreground, light) >= 4.5);
                    }
                }
                if (0.012..=0.88).contains(&base) {
                    // Eight-bit sRGB rounding has a larger luminance step in light themes.
                    assert!((light - base).abs() < 0.005, "{background:?} -> {color:?}");
                }
            }
            assert!(
                unique.len() > 128,
                "only {} colors on {background:?}",
                unique.len()
            );
        }
        assert!((contrast(0.0, 1.0) - 21.0).abs() < 0.00001);
        assert!((luminance([1.0; 3]) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn theme_changes_recolor_cached_repositories_and_monochrome_themes_have_hues() {
        let fixture = Fixture::new();
        fixture.repository(&fixture.0);
        let mut state = State::default();
        state.update(Some(&fixture.0));
        let mut colors = colors();
        let original = state.background(colors.foreground, colors.background, &colors.palette);
        std::fs::remove_dir_all(fixture.0.join(".git")).unwrap();
        state.update(Some(&fixture.0));
        assert_eq!(
            state.background(colors.foreground, colors.background, &colors.palette),
            original
        );
        colors.background = [235; 3];
        colors.foreground = [24; 3];
        let light = state.background(colors.foreground, colors.background, &colors.palette);
        assert_ne!(light, original);
        colors.palette = [[255, 0, 0]; 16];
        let red = state.background(colors.foreground, colors.background, &colors.palette);
        colors.palette = [[0, 255, 255]; 16];
        assert_ne!(
            state.background(colors.foreground, colors.background, &colors.palette),
            red
        );
        colors.palette = [[128; 3]; 16];
        let unique: std::collections::HashSet<_> = (0..256)
            .map(|index| tint(stable_hash(format!("repo-{index}").as_bytes()), &colors))
            .collect();
        assert!(unique.len() > 128);
    }
}
