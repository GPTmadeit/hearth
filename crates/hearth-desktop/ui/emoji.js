// Curated emoji set: [emoji, shortcode, extra search words]. The shortcode
// doubles as `:name:` in messages.

export const CATEGORIES = [
  {
    name: "Smileys",
    icon: "😀",
    items: [
      ["😀", "grinning", "happy smile"], ["😃", "smiley", "happy"], ["😄", "smile", "happy joy"], ["😁", "grin", "teeth"],
      ["😆", "laughing", "lol"], ["😅", "sweat_smile", "nervous"], ["🤣", "rofl", "lol rolling"], ["😂", "joy", "lol tears laugh"],
      ["🙂", "slight_smile", ""], ["🙃", "upside_down", "sarcasm"], ["😉", "wink", ""], ["😊", "blush", "happy"],
      ["😇", "innocent", "angel halo"], ["🥰", "smiling_hearts", "love"], ["😍", "heart_eyes", "love crush"], ["🤩", "star_struck", "wow"],
      ["😘", "kissing_heart", "kiss"], ["😋", "yum", "tasty"], ["😛", "stuck_out_tongue", ""], ["😜", "stuck_out_tongue_winking_eye", "crazy"],
      ["🤪", "zany", "crazy"], ["🤑", "money_mouth", "rich"], ["🤗", "hugging", "hug"], ["🤭", "hand_over_mouth", "oops"],
      ["🤫", "shushing", "quiet secret"], ["🤔", "thinking", "hmm"], ["🤐", "zipper_mouth", "secret"], ["🤨", "raised_eyebrow", "sus"],
      ["😐", "neutral", "meh"], ["😑", "expressionless", ""], ["😶", "no_mouth", "speechless"], ["😏", "smirk", ""],
      ["😒", "unamused", "meh"], ["🙄", "eye_roll", "whatever"], ["😬", "grimacing", "awkward"], ["😮‍💨", "exhale", "relief"],
      ["🤥", "lying", "pinocchio"], ["😌", "relieved", ""], ["😔", "pensive", "sad"], ["😪", "sleepy", ""],
      ["🤤", "drooling", ""], ["😴", "sleeping", "zzz"], ["😷", "mask", "sick"], ["🤒", "thermometer_face", "sick"],
      ["🤢", "nauseated", "sick gross"], ["🤮", "vomiting", "gross"], ["🥵", "hot", "heat"], ["🥶", "cold", "freezing"],
      ["🥴", "woozy", "drunk"], ["😵", "dizzy_face", ""], ["🤯", "exploding_head", "mind blown"], ["🤠", "cowboy", ""],
      ["🥳", "partying", "party celebrate"], ["😎", "sunglasses", "cool"], ["🤓", "nerd", "geek"], ["🧐", "monocle", ""],
      ["😕", "confused", ""], ["😟", "worried", ""], ["🙁", "slight_frown", "sad"], ["😮", "open_mouth", "wow"],
      ["😯", "hushed", "surprised"], ["😲", "astonished", "shocked"], ["😳", "flushed", "embarrassed"], ["🥺", "pleading", "puppy eyes please"],
      ["🥹", "holding_back_tears", "touched"], ["😦", "frowning", ""], ["😧", "anguished", ""], ["😨", "fearful", "scared"],
      ["😰", "anxious", "sweat"], ["😥", "sad_relieved", ""], ["😢", "cry", "sad tear"], ["😭", "sob", "crying sad"],
      ["😱", "scream", "horror"], ["😖", "confounded", ""], ["😣", "persevere", ""], ["😞", "disappointed", "sad"],
      ["😓", "sweat", ""], ["😩", "weary", "tired"], ["😫", "tired_face", ""], ["🥱", "yawning", "bored tired"],
      ["😤", "triumph", "huff"], ["😡", "rage", "angry mad"], ["😠", "angry", "mad"], ["🤬", "cursing", "swearing"],
      ["😈", "smiling_imp", "devil"], ["👿", "imp", "devil"], ["💀", "skull", "dead lol"], ["☠️", "skull_crossbones", "dead"],
      ["💩", "poop", ""], ["🤡", "clown", ""], ["👹", "ogre", ""], ["👻", "ghost", "boo"],
      ["👽", "alien", "ufo"], ["🤖", "robot", "bot"], ["😺", "smiley_cat", "cat"], ["😹", "joy_cat", "cat lol"],
      ["😻", "heart_eyes_cat", "cat love"], ["🙈", "see_no_evil", "monkey"], ["🙉", "hear_no_evil", "monkey"], ["🙊", "speak_no_evil", "monkey"],
    ],
  },
  {
    name: "People",
    icon: "👋",
    items: [
      ["👋", "wave", "hello hi bye"], ["🤚", "raised_back_of_hand", ""], ["✋", "raised_hand", "high five stop"], ["🖖", "vulcan", "spock"],
      ["👌", "ok_hand", "okay perfect"], ["🤌", "pinched_fingers", "italian"], ["✌️", "v", "peace victory"], ["🤞", "crossed_fingers", "luck"],
      ["🤟", "love_you_gesture", ""], ["🤘", "metal", "rock"], ["🤙", "call_me", "shaka"], ["👈", "point_left", ""],
      ["👉", "point_right", ""], ["👆", "point_up", ""], ["👇", "point_down", ""], ["☝️", "point_up_2", ""],
      ["👍", "thumbsup", "+1 yes like good"], ["👎", "thumbsdown", "-1 no dislike bad"], ["✊", "fist", ""], ["👊", "punch", "fist bump"],
      ["👏", "clap", "applause"], ["🙌", "raised_hands", "hooray"], ["👐", "open_hands", ""], ["🤲", "palms_up", ""],
      ["🤝", "handshake", "deal"], ["🙏", "pray", "please thanks"], ["✍️", "writing_hand", ""], ["💅", "nail_care", ""],
      ["💪", "muscle", "strong flex"], ["🦾", "mechanical_arm", ""], ["🧠", "brain", "smart"], ["👀", "eyes", "look"],
      ["👁️", "eye", ""], ["👅", "tongue", ""], ["👄", "lips", "mouth"], ["🫶", "heart_hands", "love"],
      ["👶", "baby", ""], ["🧒", "child", "kid"], ["👦", "boy", ""], ["👧", "girl", ""],
      ["🧑", "person", "adult"], ["👨", "man", ""], ["👩", "woman", ""], ["🧓", "older_person", ""],
      ["🧑‍💻", "technologist", "coder programmer"], ["🧑‍🎤", "singer", "rockstar"], ["🧑‍🍳", "cook", "chef"], ["🧑‍🚀", "astronaut", "space"],
      ["🥷", "ninja", ""], ["🦸", "superhero", "hero"], ["🧙", "mage", "wizard"], ["🧛", "vampire", ""],
      ["🧟", "zombie", ""], ["💃", "dancer", "dance"], ["🕺", "man_dancing", "dance"], ["🤷", "shrug", "idk dunno"],
      ["🤦", "facepalm", "ugh"], ["🙋", "raising_hand", "me"], ["🙇", "bow", ""], ["🫡", "salute", ""],
    ],
  },
  {
    name: "Nature",
    icon: "🐶",
    items: [
      ["🐶", "dog", "puppy"], ["🐱", "cat", "kitty"], ["🐭", "mouse", ""], ["🐹", "hamster", ""],
      ["🐰", "rabbit", "bunny"], ["🦊", "fox", ""], ["🐻", "bear", ""], ["🐼", "panda", ""],
      ["🐨", "koala", ""], ["🐯", "tiger", ""], ["🦁", "lion", ""], ["🐮", "cow", ""],
      ["🐷", "pig", ""], ["🐸", "frog", ""], ["🐵", "monkey_face", ""], ["🐔", "chicken", ""],
      ["🐧", "penguin", ""], ["🐦", "bird", ""], ["🦆", "duck", ""], ["🦅", "eagle", ""],
      ["🦉", "owl", ""], ["🐺", "wolf", ""], ["🐗", "boar", ""], ["🐴", "horse", ""],
      ["🦄", "unicorn", ""], ["🐝", "bee", ""], ["🐛", "bug", ""], ["🦋", "butterfly", ""],
      ["🐌", "snail", "slow"], ["🐞", "ladybug", ""], ["🐢", "turtle", "slow"], ["🐍", "snake", ""],
      ["🦖", "t_rex", "dinosaur"], ["🐙", "octopus", ""], ["🦀", "crab", "rust"], ["🐠", "tropical_fish", ""],
      ["🐬", "dolphin", ""], ["🐳", "whale", ""], ["🦈", "shark", ""], ["🐊", "crocodile", ""],
      ["🌵", "cactus", ""], ["🎄", "christmas_tree", ""], ["🌲", "evergreen_tree", ""], ["🌳", "tree", ""],
      ["🌴", "palm_tree", ""], ["🌱", "seedling", "plant grow"], ["🍀", "four_leaf_clover", "luck"], ["🍁", "maple_leaf", "fall"],
      ["🍄", "mushroom", ""], ["🌷", "tulip", "flower"], ["🌹", "rose", "flower"], ["🌻", "sunflower", "flower"],
      ["🌸", "cherry_blossom", "flower"], ["🌞", "sun_with_face", ""], ["🌝", "full_moon_face", ""], ["🌚", "new_moon_face", ""],
      ["⭐", "star", ""], ["🌟", "star2", "glow"], ["✨", "sparkles", "shiny new"], ["⚡", "zap", "lightning fast"],
      ["🔥", "fire", "lit hot"], ["🌈", "rainbow", ""], ["☀️", "sunny", "sun"], ["⛅", "partly_sunny", ""],
      ["☁️", "cloud", ""], ["🌧️", "rain", ""], ["⛈️", "thunder", "storm"], ["❄️", "snowflake", "cold"],
      ["☃️", "snowman", ""], ["🌊", "ocean", "wave water"], ["💧", "droplet", "water"], ["🌍", "earth", "world globe"],
    ],
  },
  {
    name: "Food",
    icon: "🍕",
    items: [
      ["🍏", "green_apple", ""], ["🍎", "apple", ""], ["🍊", "orange", ""], ["🍋", "lemon", ""],
      ["🍌", "banana", ""], ["🍉", "watermelon", ""], ["🍇", "grapes", ""], ["🍓", "strawberry", ""],
      ["🍒", "cherries", ""], ["🍑", "peach", ""], ["🥭", "mango", ""], ["🍍", "pineapple", ""],
      ["🥥", "coconut", ""], ["🥑", "avocado", ""], ["🍆", "eggplant", ""], ["🥕", "carrot", ""],
      ["🌽", "corn", ""], ["🌶️", "hot_pepper", "spicy"], ["🥦", "broccoli", ""], ["🥔", "potato", ""],
      ["🥐", "croissant", ""], ["🍞", "bread", ""], ["🧀", "cheese", ""], ["🥚", "egg", ""],
      ["🍳", "cooking", "fried egg"], ["🥓", "bacon", ""], ["🍔", "hamburger", "burger"], ["🍟", "fries", ""],
      ["🍕", "pizza", ""], ["🌭", "hotdog", ""], ["🌮", "taco", ""], ["🌯", "burrito", ""],
      ["🍜", "ramen", "noodles"], ["🍝", "spaghetti", "pasta"], ["🍣", "sushi", ""], ["🍱", "bento", ""],
      ["🍤", "fried_shrimp", ""], ["🍙", "rice_ball", ""], ["🍿", "popcorn", "movie"], ["🍩", "doughnut", "donut"],
      ["🍪", "cookie", ""], ["🎂", "birthday", "cake"], ["🍰", "cake", ""], ["🧁", "cupcake", ""],
      ["🍫", "chocolate_bar", ""], ["🍬", "candy", ""], ["🍭", "lollipop", ""], ["🍦", "icecream", ""],
      ["☕", "coffee", "tea"], ["🍵", "tea", ""], ["🧋", "bubble_tea", "boba"], ["🥤", "cup_with_straw", "soda"],
      ["🍺", "beer", ""], ["🍻", "beers", "cheers"], ["🥂", "champagne_glass", "cheers toast"], ["🍷", "wine_glass", "wine"],
      ["🥃", "tumbler_glass", "whiskey"], ["🍸", "cocktail", ""], ["🧃", "juice_box", ""], ["🧊", "ice", ""],
    ],
  },
  {
    name: "Activities",
    icon: "🎮",
    items: [
      ["⚽", "soccer", "football"], ["🏀", "basketball", ""], ["🏈", "football", ""], ["⚾", "baseball", ""],
      ["🎾", "tennis", ""], ["🏐", "volleyball", ""], ["🏓", "ping_pong", ""], ["🏸", "badminton", ""],
      ["🥅", "goal", ""], ["⛳", "golf", ""], ["🎣", "fishing", ""], ["🥊", "boxing", ""],
      ["🛹", "skateboard", ""], ["⛷️", "skier", "ski"], ["🏂", "snowboarder", ""], ["🏋️", "weight_lifter", "gym"],
      ["🚴", "bike", "cycling"], ["🏆", "trophy", "win"], ["🥇", "first_place", "gold"], ["🥈", "second_place", "silver"],
      ["🥉", "third_place", "bronze"], ["🏅", "medal", ""], ["🎖️", "military_medal", ""], ["🎫", "ticket", ""],
      ["🎮", "video_game", "gaming controller"], ["🕹️", "joystick", "arcade"], ["🎲", "game_die", "dice"], ["♟️", "chess_pawn", "chess"],
      ["🎯", "dart", "bullseye target"], ["🎳", "bowling", ""], ["🧩", "jigsaw", "puzzle"], ["🎰", "slot_machine", ""],
      ["🎨", "art", "paint"], ["🎬", "clapper", "movie film"], ["🎤", "microphone", "sing karaoke"], ["🎧", "headphones", "music"],
      ["🎵", "musical_note", "music"], ["🎶", "notes", "music"], ["🎹", "musical_keyboard", "piano"], ["🥁", "drum", ""],
      ["🎸", "guitar", ""], ["🎻", "violin", ""], ["🎺", "trumpet", ""], ["🎷", "saxophone", ""],
      ["🎉", "tada", "party celebrate"], ["🎊", "confetti_ball", "party"], ["🎈", "balloon", "party"], ["🎁", "gift", "present"],
    ],
  },
  {
    name: "Objects",
    icon: "💡",
    items: [
      ["⌚", "watch", ""], ["📱", "iphone", "phone mobile"], ["💻", "computer", "laptop"], ["⌨️", "keyboard", ""],
      ["🖥️", "desktop", "monitor"], ["🖱️", "mouse_three_button", "mouse"], ["💾", "floppy_disk", "save"], ["💿", "cd", ""],
      ["📷", "camera", "photo"], ["📸", "camera_flash", "photo"], ["🎥", "movie_camera", "video"], ["📺", "tv", ""],
      ["📻", "radio", ""], ["🔋", "battery", ""], ["🔌", "electric_plug", ""], ["💡", "bulb", "idea"],
      ["🔦", "flashlight", ""], ["🕯️", "candle", ""], ["🧯", "fire_extinguisher", ""], ["💸", "money_with_wings", "spend"],
      ["💰", "moneybag", "money"], ["💳", "credit_card", ""], ["💎", "gem", "diamond"], ["⚖️", "scales", "justice"],
      ["🔧", "wrench", "tool fix"], ["🔨", "hammer", "tool"], ["🛠️", "tools", "fix build"], ["⚙️", "gear", "settings"],
      ["🔩", "nut_and_bolt", ""], ["🧲", "magnet", ""], ["🔫", "water_gun", ""], ["💣", "bomb", ""],
      ["🔪", "knife", ""], ["🛡️", "shield", "security"], ["🔑", "key", ""], ["🗝️", "old_key", ""],
      ["🔒", "lock", "secure private"], ["🔓", "unlock", ""], ["🚪", "door", ""], ["🛏️", "bed", "sleep"],
      ["🧸", "teddy_bear", ""], ["🎀", "ribbon", ""], ["✉️", "envelope", "mail"], ["📦", "package", "box"],
      ["📝", "memo", "note write"], ["📁", "file_folder", "folder"], ["📅", "calendar", "date"], ["📌", "pushpin", "pin"],
      ["📎", "paperclip", "attach"], ["✂️", "scissors", "cut"], ["🗑️", "wastebasket", "trash delete"], ["📚", "books", "read"],
      ["🔔", "bell", "notification"], ["🔕", "no_bell", "mute"], ["📣", "mega", "announce"], ["⏰", "alarm_clock", "time"],
      ["⏳", "hourglass", "wait"], ["🚀", "rocket", "launch ship"], ["✈️", "airplane", "travel"], ["🚗", "car", ""],
      ["🏠", "house", "home"], ["🏰", "castle", ""], ["⛺", "tent", "camping"], ["🗺️", "map", ""],
    ],
  },
  {
    name: "Symbols",
    icon: "❤️",
    items: [
      ["❤️", "heart", "love red"], ["🧡", "orange_heart", ""], ["💛", "yellow_heart", ""], ["💚", "green_heart", ""],
      ["💙", "blue_heart", ""], ["💜", "purple_heart", ""], ["🖤", "black_heart", ""], ["🤍", "white_heart", ""],
      ["🤎", "brown_heart", ""], ["💔", "broken_heart", "sad"], ["❤️‍🔥", "heart_on_fire", ""], ["💕", "two_hearts", "love"],
      ["💖", "sparkling_heart", ""], ["💗", "heartpulse", ""], ["💯", "100", "hundred perfect"], ["💢", "anger", ""],
      ["💥", "boom", "explosion"], ["💫", "dizzy", ""], ["💦", "sweat_drops", ""], ["💨", "dash", "fast"],
      ["💬", "speech_balloon", "chat"], ["💭", "thought_balloon", "think"], ["💤", "zzz", "sleep"], ["✅", "white_check_mark", "done yes check"],
      ["☑️", "ballot_box_with_check", ""], ["✔️", "heavy_check_mark", "check"], ["❌", "x", "no wrong cross"], ["❎", "negative_squared_cross_mark", ""],
      ["➕", "heavy_plus_sign", "plus add"], ["➖", "heavy_minus_sign", "minus"], ["❓", "question", ""], ["❗", "exclamation", "important"],
      ["‼️", "bangbang", ""], ["⁉️", "interrobang", ""], ["⚠️", "warning", "caution"], ["🚫", "no_entry_sign", "forbidden"],
      ["⛔", "no_entry", ""], ["🔞", "underage", ""], ["♻️", "recycle", ""], ["🔴", "red_circle", ""],
      ["🟠", "orange_circle", ""], ["🟡", "yellow_circle", ""], ["🟢", "green_circle", ""], ["🔵", "blue_circle", ""],
      ["🟣", "purple_circle", ""], ["⚫", "black_circle", ""], ["⚪", "white_circle", ""], ["🔺", "small_red_triangle", ""],
      ["▶️", "arrow_forward", "play"], ["⏸️", "pause", ""], ["⏹️", "stop", ""], ["🔁", "repeat", "loop"],
      ["➡️", "arrow_right", ""], ["⬅️", "arrow_left", ""], ["⬆️", "arrow_up", ""], ["⬇️", "arrow_down", ""],
      ["🆗", "ok", ""], ["🆕", "new", ""], ["🆒", "cool", ""], ["🆘", "sos", "help"],
      ["™️", "tm", "trademark"], ["©️", "copyright", ""], ["🏳️‍🌈", "rainbow_flag", "pride"], ["🏁", "checkered_flag", "finish race"],
    ],
  },
];

export const ALL = CATEGORIES.flatMap((c) => c.items);

const BY_CODE = new Map();
for (const [emoji, code] of ALL) BY_CODE.set(code, emoji);
// Common aliases people type out of habit.
for (const [alias, code] of [["+1", "thumbsup"], ["-1", "thumbsdown"], ["thumbs_up", "thumbsup"], ["lol", "joy"], ["heart_eyes", "heart_eyes"], ["ok", "ok_hand"], ["sad", "cry"], ["laugh", "laughing"], ["party", "tada"], ["shrug", "shrug"], ["eyes", "eyes"]]) {
  if (BY_CODE.has(code)) BY_CODE.set(alias, BY_CODE.get(code));
}

/** Replaces `:shortcode:` with emoji, leaving unknown codes and code spans alone. */
export function expandShortcodes(text) {
  return text.replace(/(`[^`]*`)|:([a-z0-9_+-]{1,40}):/gi, (match, code, name) => code || BY_CODE.get(name.toLowerCase()) || match);
}

/** Emoji whose shortcode or keywords match `query`. */
export function search(query) {
  const q = query.trim().toLowerCase();
  if (!q) return ALL;
  return ALL.filter(([, code, words]) => code.includes(q) || words.includes(q));
}
