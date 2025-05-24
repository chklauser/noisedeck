# CLAUDE.md - Noisedeck Project Guide

## Project Overview

Noisedeck is a specialized replacement for Elgato Stream Deck controller software with a singular focus: playing ambient sounds and background music for tabletop RPG sessions. Unlike the official extensible platform, Noisedeck deliberately limits its scope to audio playback for better performance and user experience.

## Architecture

The application is built with a three-component architecture that communicates via async message passing:

### Core Components

1. **Hardware Controller (`daemon.rs`)**
   - Controls Elgato Stream Deck hardware
   - Handles button rendering and display updates
   - Manages physical device interactions

2. **UI Logic (`daemon/ui.rs`)**
   - High-level application logic
   - Button press event handling
   - Page navigation and state management
   - Coordinating between hardware and audio components

3. **Audio Engine (`daemon/audio.rs`)**
   - Audio playback control using Kira audio library
   - Playback event reporting (position, start/stop)
   - Volume control and fade effects
   - Multiple playback modes (play/stop, overlap, loop)

### Data Flow
- User presses Stream Deck button → Hardware Controller → UI Logic → Audio Engine
- Audio events flow back: Audio Engine → UI Logic → Hardware Controller (for visual feedback)

## Technology Stack

- **Language**: Rust (2024 edition)
- **Async Runtime**: Tokio
- **Audio**: Kira (with cpal backend, MP3 support)
- **Hardware**: elgato-streamdeck crate
- **Graphics**: image, imageproc, cosmic-text for button rendering
- **Config**: Serde JSON for serialization
- **CLI**: clap for command-line interface
- **Error Handling**: eyre/stable-eyre

## Configuration System

### Internal Format
The application uses a hierarchical configuration structure:
- `Config`: Top-level container with pages and start page
- `Page`: Named collection of buttons
- `Button`: Individual button with label and behavior
- `ButtonBehavior`: Either page navigation or sound playback

### Import System
Located in `src/import/`, this handles converting Elgato Stream Deck configuration files (ZIP format) into Noisedeck's internal format. The import process:
1. Extracts ZIP archives
2. Parses Elgato's JSON configuration
3. Maps actions to Noisedeck's simplified sound-only model
4. Preserves folder hierarchy as pages

## Development Practices

### Cross-Platform Support
- **Primary Target**: Linux (specifically Fedora)
- **Secondary Target**: Windows (development platform only)
- Use platform-agnostic dependencies where possible
- Test audio functionality on both platforms

### Performance Considerations
- Audio libraries (Kira, cpal, symphonia) are optimized even in debug builds
- Async/await throughout for responsive UI
- Arc<T> used for shared data to minimize clones

### Code Organization
```
src/
├── main.rs           # CLI parsing and application entry
├── daemon.rs         # Hardware controller module
├── daemon/
│   ├── ui.rs         # UI logic and state management
│   └── audio.rs      # Audio playback engine
├── import.rs         # Elgato config import module
├── import/           # Import implementation details
└── util.rs           # Shared utilities
```

## Key Design Decisions

1. **Single Purpose**: Audio-only functionality instead of extensible plugin system
2. **Async Architecture**: All components communicate via async messages
3. **Immutable Config**: Configuration uses Arc<T> for efficient sharing
4. **Error Handling**: eyre for user-friendly error messages
5. **Logging**: tracing for structured logging with environment-based filtering

## Working with the Codebase

### Adding New Features
- Audio-related features go in `daemon/audio.rs`
- UI logic and button handling in `daemon/ui.rs`
- Hardware/display changes in `daemon.rs`
- New import formats in `import/`

### Configuration Changes
- Modify structs in `mod config` section of `main.rs`
- Ensure Serde compatibility for serialization
- Update import logic if needed

### Dependencies
- Keep audio dependencies optimized for performance
- Prefer async-compatible crates
- Minimize runtime dependencies for deployment

### Testing
- Focus on _our_ logic. We don't need to test whether a dependency, such as Krita, tokio or elgato-streamdeck, works correctly. We assume that our dependencies are well tested.'
- Prefer high-level "integration" tests over unit tests (not integration tests in the Cargo `tests/` sense, but integration tests in the sense that we want to test as much of the system at once as possible)
- The previous two goals are obviously in tension. I'd rather have tests of the `daemon/ui.rs` component as a whole than creating a complex test harness _just_ to fake streamdeck and audio hardware.

## Environment Setup

### Environment Variables
- Use `.env` file for development configuration
- `RUST_LOG` for tracing level control
- Check `dotenvy` integration in main.rs

## Common Patterns

### Error Handling
- Use `eyre::Result<T>` for fallible operations
- Convert errors with `?` operator
- Provide context with `.wrap_err()` when helpful

### Async Communication
- Components communicate via tokio channels
- Use `Arc<T>` for shared immutable data
- Prefer `tokio::spawn` for concurrent tasks
- Use `tokio::task::spawn_blocking` for blocking operations
- Use `tokio` concurrency primitives for synchronization in async code

### Reliability
- The application must not crash/panic
- The application should try to recover from errors and restore itself into a working state

### Code style
- Use explicit type state (e.g., prefer an enum or a record over a naked boolean)
- Comments must add insight into why code is written in a particular way. I don't want comments that just repeat _what_ is happening or _how_ somehting is happening (I can read the code for that).
- Comments must not repeat information that is already conveyed by type signatures.
- If some values have special meaning (true, false, zero, negative numbers), that's worth mentioning in a comment.
- Prefer standard Rust language features (traits, generics, dyn) over macros whenever possible
- Macros are allowed as a solution, but their scope should be kept minimal (as in: the macro arguments should contain as little code as possible). Reason: the editor experience inside macro arguments is not great. Less code within macros results in a better editing experience.
