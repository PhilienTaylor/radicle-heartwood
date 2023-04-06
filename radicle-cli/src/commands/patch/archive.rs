use super::*;

use radicle::cob::patch;
use radicle::prelude::*;
use radicle::storage::git::Repository;

pub fn run(repository: &Repository, profile: &Profile, patch_id: &PatchId) -> anyhow::Result<()> {
    let signer = term::signer(profile)?;
    let mut patches = patch::Patches::open(repository)?;
    let Ok(mut patch) = patches.get_mut(patch_id) else {
        anyhow::bail!("Patch `{patch_id}` not found");
    };
    patch.archive(&signer)?;

    Ok(())
}
