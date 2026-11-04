//! Emoji catalogue for the icon picker (name keywords make the search work). Any other emoji can be typed or pasted.

pub type Cat = (&'static str, &'static str, &'static [(&'static str, &'static str)]);

pub const CATS: &[Cat] = &[
    ("Smileys", "😀", &[
        ("😀", "grin happy smile"), ("😃", "smile happy"), ("😄", "smile laugh"), ("😁", "beam grin"), ("😆", "laugh squint"), ("😅", "sweat smile"),
        ("🤣", "rofl laugh"), ("😂", "joy tears laugh"), ("🙂", "slight smile"), ("😉", "wink"), ("😊", "blush smile"), ("😇", "angel halo"),
        ("🥰", "love hearts face"), ("😍", "heart eyes love"), ("🤩", "star struck wow"), ("😘", "kiss"), ("😋", "yum tasty"), ("😜", "wink tongue"),
        ("🤪", "crazy zany"), ("🤗", "hug"), ("🤔", "think hmm"), ("🤨", "eyebrow skeptical"), ("😐", "neutral"), ("😑", "expressionless"),
        ("😏", "smirk"), ("😒", "unamused"), ("🙄", "eye roll"), ("😬", "grimace"), ("😌", "relieved calm"), ("😴", "sleep zzz"),
        ("😎", "cool sunglasses"), ("🤓", "nerd glasses"), ("🧐", "monocle"), ("😕", "confused"), ("😟", "worried"), ("😢", "cry sad"),
        ("😭", "sob cry"), ("😤", "triumph huff"), ("😡", "angry mad"), ("🤯", "mind blown"), ("😱", "scream fear"), ("🥳", "party celebrate"),
        ("🤠", "cowboy"), ("🥺", "pleading"), ("😈", "devil"), ("💀", "skull dead"), ("👻", "ghost"), ("🤖", "robot"), ("💩", "poop"),
    ]),
    ("People", "👋", &[
        ("👋", "wave hello"), ("🤚", "hand raised"), ("✋", "stop hand"), ("👌", "ok"), ("🤌", "pinched"), ("✌️", "peace victory"), ("🤞", "fingers crossed luck"),
        ("🤟", "love you"), ("🤘", "rock"), ("👈", "point left"), ("👉", "point right"), ("👆", "point up"), ("👇", "point down"), ("👍", "thumbs up like yes"),
        ("👎", "thumbs down no"), ("👏", "clap applause"), ("🙌", "praise hooray"), ("🤝", "handshake deal"), ("🙏", "pray thanks please"), ("💪", "muscle strong"),
        ("🧠", "brain mind"), ("👀", "eyes look"), ("👤", "person user"), ("👥", "people team users"), ("🧑‍💻", "developer coder technologist"), ("👩‍💻", "woman developer"),
        ("👨‍💻", "man developer"), ("🧑‍🎨", "artist designer"), ("🧑‍🏫", "teacher"), ("🧑‍🚀", "astronaut"), ("🦸", "hero superhero"), ("🧙", "wizard mage"),
    ]),
    ("Nature", "🌱", &[
        ("🐶", "dog puppy"), ("🐱", "cat kitten"), ("🐭", "mouse"), ("🐰", "rabbit bunny"), ("🦊", "fox"), ("🐻", "bear"), ("🐼", "panda"), ("🐨", "koala"),
        ("🐯", "tiger"), ("🦁", "lion"), ("🐮", "cow"), ("🐷", "pig"), ("🐸", "frog"), ("🐵", "monkey"), ("🐔", "chicken"), ("🐧", "penguin"),
        ("🐦", "bird"), ("🦉", "owl wise"), ("🦄", "unicorn"), ("🐝", "bee"), ("🦋", "butterfly"), ("🐢", "turtle"), ("🐙", "octopus"), ("🐬", "dolphin"),
        ("🐳", "whale"), ("🦈", "shark"), ("🐘", "elephant"), ("🦒", "giraffe"), ("🐉", "dragon"), ("🦖", "dinosaur trex"), ("🌱", "seedling sprout grow"), ("🌿", "herb leaf"),
        ("🍀", "clover luck"), ("🌲", "tree pine"), ("🌳", "tree"), ("🌴", "palm"), ("🌵", "cactus"), ("🌸", "blossom flower"), ("🌹", "rose"), ("🌻", "sunflower"),
        ("🍁", "maple leaf autumn"), ("🌍", "earth globe world"), ("🌙", "moon night"), ("⭐", "star"), ("🌟", "glowing star"), ("☀️", "sun"), ("⛅", "cloud sun"),
        ("🌈", "rainbow"), ("❄️", "snow cold"), ("🔥", "fire hot flame"), ("💧", "drop water"), ("🌊", "wave ocean"),
    ]),
    ("Food", "🍕", &[
        ("🍎", "apple"), ("🍊", "orange"), ("🍋", "lemon"), ("🍌", "banana"), ("🍉", "watermelon"), ("🍇", "grapes"), ("🍓", "strawberry"), ("🍒", "cherries"),
        ("🍑", "peach"), ("🥭", "mango"), ("🍍", "pineapple"), ("🥑", "avocado"), ("🥕", "carrot"), ("🌽", "corn"), ("🥦", "broccoli"), ("🍞", "bread"),
        ("🧀", "cheese"), ("🥚", "egg"), ("🥓", "bacon"), ("🍔", "burger"), ("🍟", "fries"), ("🍕", "pizza"), ("🌮", "taco"), ("🍣", "sushi"),
        ("🍜", "noodles ramen"), ("🍝", "pasta"), ("🍰", "cake"), ("🎂", "birthday cake"), ("🍩", "donut"), ("🍪", "cookie"), ("🍫", "chocolate"), ("🍿", "popcorn"),
        ("☕", "coffee tea hot"), ("🍵", "tea matcha"), ("🍺", "beer"), ("🍷", "wine"), ("🥤", "drink soda"), ("🧃", "juice"),
    ]),
    ("Activities", "⚽", &[
        ("⚽", "soccer football"), ("🏀", "basketball"), ("🏈", "american football"), ("⚾", "baseball"), ("🎾", "tennis"), ("🏐", "volleyball"), ("🏓", "ping pong"),
        ("🥊", "boxing"), ("🏆", "trophy win"), ("🥇", "gold medal first"), ("🎯", "target bullseye goal"), ("🎮", "game controller video"), ("🎲", "dice game"), ("🧩", "puzzle piece"),
        ("🎨", "art palette paint"), ("🎬", "clapper movie film"), ("🎤", "microphone sing"), ("🎧", "headphones music"), ("🎸", "guitar"), ("🎹", "piano keyboard"),
        ("🎻", "violin"), ("🎵", "music note"), ("🎭", "theatre masks"), ("🎪", "circus tent"), ("🎉", "party popper tada"), ("🎁", "gift present"), ("🏋️", "weights gym"),
        ("🚴", "cycling bike"), ("🏊", "swim"), ("🧘", "yoga meditate"), ("🏔️", "mountain"), ("⛺", "camping tent"),
    ]),
    ("Travel", "🚀", &[
        ("🚗", "car"), ("🚕", "taxi"), ("🚌", "bus"), ("🚑", "ambulance"), ("🚒", "fire truck"), ("🚚", "truck delivery"), ("🚲", "bicycle"), ("🏍️", "motorcycle"),
        ("✈️", "plane airplane flight travel"), ("🚀", "rocket launch startup"), ("🛸", "ufo"), ("🚁", "helicopter"), ("⛵", "sailboat"), ("🚢", "ship"), ("🚂", "train"),
        ("🏠", "house home"), ("🏡", "house garden"), ("🏢", "office building"), ("🏫", "school"), ("🏥", "hospital"), ("🏦", "bank"), ("🏰", "castle"),
        ("🗼", "tower"), ("🗽", "statue liberty"), ("🌆", "city"), ("🌃", "night city"), ("🗺️", "map world"), ("🧭", "compass navigate"), ("⛽", "fuel gas"), ("🚦", "traffic light"),
    ]),
    ("Objects", "💡", &[
        ("💡", "idea bulb light"), ("🔦", "flashlight"), ("📱", "phone mobile"), ("💻", "laptop computer"), ("🖥️", "desktop computer"), ("⌨️", "keyboard"), ("🖱️", "mouse"),
        ("🖨️", "printer"), ("📷", "camera photo"), ("📹", "video camera"), ("📺", "tv"), ("📻", "radio"), ("⏰", "alarm clock time"), ("⌛", "hourglass time"), ("🔋", "battery"),
        ("🔌", "plug"), ("💾", "floppy save disk"), ("💿", "cd disc"), ("📄", "page document file"), ("📃", "page curl"), ("📑", "bookmark tabs"), ("📊", "chart bar stats"),
        ("📈", "chart up growth"), ("📉", "chart down"), ("📋", "clipboard list"), ("📅", "calendar date"), ("📆", "calendar"), ("📁", "folder"), ("📂", "open folder"),
        ("🗂️", "dividers index"), ("🗃️", "file box"), ("🗄️", "cabinet"), ("🗑️", "trash bin delete"), ("📝", "memo note write"), ("✏️", "pencil"), ("🖊️", "pen"),
        ("📌", "pin"), ("📎", "paperclip attach"), ("📏", "ruler"), ("✂️", "scissors cut"), ("🔒", "lock secure"), ("🔓", "unlock"), ("🔑", "key"),
        ("🔨", "hammer"), ("🛠️", "tools wrench build"), ("⚙️", "gear settings cog"), ("🔧", "wrench"), ("🧰", "toolbox"), ("🔬", "microscope science"), ("🔭", "telescope"),
        ("📚", "books library study"), ("📖", "open book read"), ("📓", "notebook"), ("📔", "notebook decorative"), ("📒", "ledger"), ("🔖", "bookmark"), ("📰", "news newspaper"),
        ("💰", "money bag"), ("💵", "dollar cash"), ("💳", "credit card"), ("💎", "gem diamond"), ("🛒", "cart shopping"), ("🎓", "graduation cap school"), ("🔔", "bell notification"),
        ("📣", "megaphone announce"), ("💬", "speech chat comment"), ("✉️", "mail envelope email"), ("📦", "package box"), ("🔍", "search magnifier find"), ("🧪", "test tube experiment"),
    ]),
    ("Symbols", "❤️", &[
        ("❤️", "red heart love"), ("🧡", "orange heart"), ("💛", "yellow heart"), ("💚", "green heart"), ("💙", "blue heart"), ("💜", "purple heart"), ("🖤", "black heart"),
        ("🤍", "white heart"), ("💔", "broken heart"), ("💯", "hundred perfect"), ("✅", "check done yes"), ("☑️", "check box"), ("❌", "cross no wrong"), ("❓", "question"),
        ("❗", "exclamation important"), ("⚠️", "warning caution"), ("🚫", "prohibited no"), ("♻️", "recycle"), ("⚡", "zap lightning bolt fast"), ("✨", "sparkles magic new"),
        ("💫", "dizzy"), ("💥", "boom collision"), ("💤", "zzz sleep"), ("🔴", "red circle"), ("🟠", "orange circle"), ("🟡", "yellow circle"), ("🟢", "green circle"),
        ("🔵", "blue circle"), ("🟣", "purple circle"), ("⚫", "black circle"), ("⚪", "white circle"), ("🟥", "red square"), ("🟩", "green square"), ("🟦", "blue square"),
        ("▶️", "play"), ("⏸️", "pause"), ("🔁", "repeat loop"), ("🔀", "shuffle"), ("➕", "plus add"), ("➖", "minus"), ("➡️", "arrow right"), ("⬆️", "arrow up"),
        ("🏁", "finish flag race"), ("🚩", "flag red"), ("🏳️", "white flag"), ("©️", "copyright"), ("™️", "trademark"), ("♾️", "infinity"), ("🆕", "new"), ("🆗", "ok"),
    ]),
];

/// All emoji matching `q` (case-insensitive substring of the keywords or the emoji itself).
pub fn search(q: &str) -> Vec<&'static str> {
    let q = q.trim().to_lowercase();
    CATS.iter().flat_map(|c| c.2.iter()).filter(|(e, k)| k.contains(&q) || *e == q).map(|(e, _)| *e).collect()
}

/// First grapheme-ish cluster of user input that looks like an emoji (handles ZWJ sequences, variation selectors, skin tones, flags).
pub fn first_emoji(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = s.trim().chars().peekable();
    let is_emoji_base = |c: char| (c as u32) >= 0x2190 && !c.is_alphanumeric();
    let first = chars.next()?;
    if !is_emoji_base(first) {
        return None;
    }
    out.push(first);
    let regional = |c: char| ('\u{1F1E6}'..='\u{1F1FF}').contains(&c);
    if regional(first) {
        if let Some(n) = chars.next().filter(|c| regional(*c)) {
            out.push(n);
        }
        return Some(out);
    }
    while let Some(&c) = chars.peek() {
        let join = c == '\u{200d}' || c == '\u{fe0f}' || ('\u{1F3FB}'..='\u{1F3FF}').contains(&c) || c == '\u{20e3}';
        if join {
            out.push(c);
            chars.next();
            if c == '\u{200d}' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
        } else {
            break;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_and_parse() {
        assert!(search("rocket").contains(&"🚀"));
        assert!(search("🚀").contains(&"🚀"));
        assert_eq!(first_emoji("  🐙 octopus").as_deref(), Some("🐙"));
        assert_eq!(first_emoji("👩‍💻 dev").as_deref(), Some("👩‍💻"));
        assert_eq!(first_emoji("🇩🇪x").as_deref(), Some("🇩🇪"));
        assert_eq!(first_emoji("hello"), None);
    }
}
