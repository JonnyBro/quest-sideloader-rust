//! Which mentions of the package are the app's identity and which are Java
//! class names. Only the identity changes: native code finds Java classes by
//! their original names, so those must stay.

fn is_ident(c: char) -> bool {
	c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// `Main`, `MainActivity`: a capital followed by lower case somewhere.
/// `C2D_MESSAGE` and `ACTION_X` are permission and action names, not classes.
fn is_class_segment(segment: &str) -> bool {
	segment.starts_with(|c: char| c.is_ascii_uppercase())
		&& segment.chars().any(|c| c.is_ascii_lowercase())
}

const PLACEHOLDERS: [&str; 2] = ["${applicationId}", "${packageName}"];

/// `value` with every identity mention of `old` replaced by `new`, or `None`
/// if there is none. `com.game` and `com.game.fileprovider` count;
/// `com.gamepad`, `org.com.game` and `com.game.ui.Main` don't. Unexpanded
/// Gradle placeholders count too, or both apps would claim the same name.
pub fn rename_value(value: &str, old: &str, new: &str) -> Option<String> {
	let mut out = String::with_capacity(value.len());
	let mut changed = false;
	let mut rest = value;
	while !rest.is_empty() {
		if let Some(placeholder) = PLACEHOLDERS.iter().find(|p| rest.starts_with(**p)) {
			out.push_str(new);
			rest = &rest[placeholder.len()..];
			changed = true;
			continue;
		}
		let consumed = value.len() - rest.len();
		let before_ok = value[..consumed]
			.chars()
			.next_back()
			.is_none_or(|c| !is_ident(c) && c != '.');
		if before_ok && rest.starts_with(old) && is_identity(&rest[old.len()..]) {
			out.push_str(new);
			rest = &rest[old.len()..];
			changed = true;
			continue;
		}
		let c = rest.chars().next().unwrap_or_default();
		out.push(c);
		rest = &rest[c.len_utf8()..];
	}
	changed.then_some(out)
}

pub fn mentions(value: &str, package: &str) -> bool {
	rename_value(value, package, package).is_some()
}

/// `after` is what follows a mention of the package.
fn is_identity(after: &str) -> bool {
	match after.chars().next() {
		None => true,
		Some('.') => {
			let tail = &after[1..];
			let end = tail
				.find(|c: char| !is_ident(c) && c != '.')
				.unwrap_or(tail.len());
			!tail[..end].split('.').any(is_class_segment)
		}
		Some(c) => !is_ident(c),
	}
}

/// A component name written relative to the package (`.Main`, or `Main`
/// without dots), spelled out in full so it keeps pointing at the same class.
pub fn expand_class(name: &str, package: &str) -> Option<String> {
	if name.starts_with('.') {
		Some(format!("{package}{name}"))
	} else if !name.is_empty() && !name.contains('.') {
		Some(format!("{package}.{name}"))
	} else {
		None
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn r(value: &str) -> Option<String> {
		rename_value(value, "com.example.game", "com.example.copy")
	}

	#[test]
	fn renames_identity_only() {
		assert_eq!(r("com.example.game").as_deref(), Some("com.example.copy"));
		assert_eq!(
			r("com.example.game.fileprovider").as_deref(),
			Some("com.example.copy.fileprovider")
		);
		assert_eq!(
			r("com.example.game.permission.C2D_MESSAGE").as_deref(),
			Some("com.example.copy.permission.C2D_MESSAGE")
		);
		assert_eq!(
			r("content://com.example.game.provider/x").as_deref(),
			Some("content://com.example.copy.provider/x")
		);
		assert_eq!(
			r("${applicationId}.files").as_deref(),
			Some("com.example.copy.files")
		);
		assert_eq!(r("com.example.gamepad"), None);
		assert_eq!(r("org.foo.com.example.game"), None);
		assert_eq!(r("com.example.game.ui.MainActivity"), None);
		assert_eq!(r("com.example.game.Main"), None);
		assert_eq!(r("android.intent.action.MAIN"), None);
	}

	#[test]
	fn expands_relative_classes() {
		assert_eq!(
			expand_class(".Main", "com.a").as_deref(),
			Some("com.a.Main")
		);
		assert_eq!(expand_class("Main", "com.a").as_deref(), Some("com.a.Main"));
		assert_eq!(
			expand_class("com.unity3d.player.UnityPlayerActivity", "com.a"),
			None
		);
	}
}
