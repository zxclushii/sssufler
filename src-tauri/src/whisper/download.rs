use crate::{error::AppError, paths::remove_file};
use anyhow::{bail, Context, Result};
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub(super) struct FileSpec {
    pub name: &'static str,
    pub bytes: u64,
    pub hash: &'static str,
}
const MODELS: &[FileSpec] = &[
    FileSpec {
        name: "ggml-base-q5_1.bin",
        bytes: 59707625,
        hash: "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898",
    },
    FileSpec {
        name: "ggml-small-q5_1.bin",
        bytes: 190085487,
        hash: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
    },
    FileSpec {
        name: "ggml-small.bin",
        bytes: 487601967,
        hash: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
    },
    FileSpec {
        name: "ggml-medium-q5_0.bin",
        bytes: 539212467,
        hash: "19fea4b380c3a618ec4723c3eef2eb785ffba0d0538cf43f8f235e7b3b34220f",
    },
    FileSpec {
        name: "ggml-large-v3-turbo-q5_0.bin",
        bytes: 574041195,
        hash: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    },
    FileSpec {
        name: "ggml-silero-v6.2.0.bin",
        bytes: 885098,
        hash: "2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987",
    },
];
#[cfg(target_arch = "aarch64")]
const ARCHIVE: FileSpec = FileSpec {
    name: "whisper-bin-win-cpu-arm64.zip",
    bytes: 4371193,
    hash: "28c37e7b598c3d9bbfef94f3bd67f2ee6f12b7c86f3da5edcf5e4308d615ff6d",
};
#[cfg(not(target_arch = "aarch64"))]
const ARCHIVE: FileSpec = FileSpec {
    name: "whisper-bin-x64.zip",
    bytes: 8928640,
    hash: "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb",
};
const SILERO: &str = "ggml-silero-v6.2.0.bin";

pub(crate) fn client(read_timeout: Duration) -> Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(read_timeout)
        .build()
        .context("не создал HTTP-клиент загрузок")
}
fn matches_hash(hash: Sha256, expected: &str) -> bool {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    hash.finalize()
        .iter()
        .flat_map(|&byte| [HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]])
        .eq(expected.bytes())
}

fn disk(err: io::Error, bytes: u64) -> anyhow::Error {
    if err.kind() == io::ErrorKind::StorageFull
        || matches!(err.raw_os_error(), Some(112 | 39)) && cfg!(windows)
    {
        AppError::StorageFull(bytes.div_ceil(1048576).max(1)).into()
    } else {
        err.into()
    }
}
struct PartialFile {
    path: PathBuf,
    file: Option<File>,
    committed: bool,
}
impl Drop for PartialFile {
    fn drop(&mut self) {
        drop(self.file.take());
        if !self.committed {
            if let Err(e) = remove_file(&self.path) {
                tracing::warn!(path=%self.path.display(),"не удалил частичную загрузку: {e}");
            }
        }
    }
}
async fn fetch(
    client: &Client,
    url: &str,
    dest: &Path,
    spec: Option<FileSpec>,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<()> {
    let name = dest
        .file_name()
        .context("нет имени файла загрузки")?
        .to_string_lossy();
    let part = dest.with_file_name(format!("{name}.part"));
    remove_file(&part).with_context(|| format!("не удалил {}", part.display()))?;
    let mut res = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("не подключился к {url}"))?
        .error_for_status()
        .with_context(|| format!("не скачал {name} с {url}"))?;
    let total = spec.map(|s| s.bytes).or(res.content_length());
    let file = File::create_new(&part)
        .map_err(|e| disk(e, total.unwrap_or(0)))
        .with_context(|| format!("не создал {}", part.display()))?;
    let mut partial = PartialFile {
        path: part,
        file: Some(file),
        committed: false,
    };
    let mut received = 0;
    let mut last = None;
    let mut hasher = Sha256::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| {
        if e.is_timeout() {
            anyhow::anyhow!("не скачал {name} с {url}: превышен таймаут чтения без данных ({e})")
        } else {
            anyhow::anyhow!("оборвалась загрузка {name} с {url}: {e}")
        }
    })? {
        partial
            .file
            .as_mut()
            .context("частичный файл уже закрыт")?
            .write_all(&chunk)
            .map_err(|e| disk(e, total.unwrap_or(0).saturating_sub(received)))?;
        hasher.update(&chunk);
        received += chunk.len() as u64;
        if last.is_none_or(|t: Instant| t.elapsed() >= Duration::from_millis(250)) {
            progress(received, total);
            last = Some(Instant::now());
        }
    }
    let mut file = partial.file.take().context("загрузка потеряла файл")?;
    file.flush()
        .and_then(|()| file.sync_all())
        .map_err(|e| disk(e, total.unwrap_or(0)))?;
    drop(file);
    let valid = total.is_none_or(|n| n == received)
        && match spec {
            Some(s) => matches_hash(hasher, s.hash),
            None => received > 10 * 1024 * 1024,
        };
    if !valid {
        return Err(AppError::HashMismatch(name.into_owned()).into());
    }
    fs::rename(&partial.path, dest)
        .map_err(|e| disk(e, received))
        .with_context(|| format!("не заменил {}", dest.display()))?;
    partial.committed = true;
    Ok(())
}
#[cfg(windows)]
async fn cached_model(path: &Path, spec: Option<FileSpec>) -> Result<bool> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || -> Result<bool> {
        let mut f = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        let size = f.metadata()?.len();
        let Some(spec) = spec else {
            return Ok(size > 10 * 1024 * 1024);
        };
        if size != spec.bytes {
            return Ok(false);
        }
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        Ok(matches_hash(hash, spec.hash))
    })
    .await
    .context("прервалась проверка модели")?
}

#[cfg(windows)]
fn find_server(dir: &Path, depth: usize) -> Result<Option<PathBuf>> {
    if !dir.try_exists()? {
        return Ok(None);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_file() && entry.file_name().eq_ignore_ascii_case("whisper-server.exe") {
            return Ok(Some(entry.path()));
        }
        if ty.is_dir() && depth > 0 {
            if let Some(file) = find_server(&entry.path(), depth - 1)? {
                return Ok(Some(file));
            }
        }
    }
    Ok(None)
}

#[cfg(windows)]
pub(super) async fn assets(
    dir: &Path,
    model: &str,
    client: &Client,
    mut progress: impl FnMut(&str, Option<u64>),
) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let bin_dir = dir.join("whisper/bin");
    // The version release links to build b5454 at the same commit; the zip assets live on that tag.
    let pinned = bin_dir.join(format!("b5454-{}", std::env::consts::ARCH));
    let exe = if let Some(exe) = find_server(&pinned, 3)? {
        exe
    } else {
        let archive = dir.join("whisper").join(ARCHIVE.name);
        let url = format!(
            "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/{}",
            ARCHIVE.name
        );
        fetch(client, &url, &archive, Some(ARCHIVE), |n, t| {
            progress("качаю Whisper", t.map(|v| 100 * n / v))
        })
        .await?;
        let temp = bin_dir.join(format!("unpack-{}", std::env::consts::ARCH));
        if temp.try_exists()? {
            fs::remove_dir_all(&temp).context("не очистил незавершённую распаковку")?;
        }
        fs::create_dir_all(&temp)?;
        progress("распаковываю Whisper", None);
        let sys = std::env::var_os("SystemRoot").context("нет SystemRoot")?;
        let mut cmd = tokio::process::Command::new(PathBuf::from(sys).join("System32/tar.exe"));
        cmd.arg("-xf").arg(&archive).arg("-C").arg(&temp);
        let mut tar = super::engine::job(cmd)?;
        let status = tokio::time::timeout(Duration::from_secs(90), tar.wait())
            .await
            .context("истекло время распаковки Whisper")??;
        if !status.success() {
            bail!("tar.exe не распаковал {}: {status}", archive.display());
        }
        let file = find_server(&temp, 3)?.context("в архиве нет whisper-server.exe")?;
        let relative = file.strip_prefix(&temp)?.to_owned();
        if pinned.try_exists()? {
            fs::remove_dir_all(&pinned)?;
        }
        fs::rename(&temp, &pinned).context("не установил распакованный Whisper")?;
        remove_file(&archive).context("не удалил проверенный архив Whisper")?;
        pinned.join(relative)
    };
    let models = dir.join("whisper/models");
    let whisper = format!("ggml-{model}.bin");
    for name in [whisper.as_str(), SILERO] {
        let dest = models.join(name);
        let spec = MODELS.iter().find(|s| s.name == name).copied();
        if spec.is_none() {
            tracing::warn!(name, "модель вне таблицы SHA-256, проверяю только размер");
        }
        if cached_model(&dest, spec).await? {
            continue;
        }
        let repo = if name == SILERO {
            "ggml-org/whisper-vad"
        } else {
            "ggerganov/whisper.cpp"
        };
        let url = format!("https://huggingface.co/{repo}/resolve/main/{name}");
        let label = format!("качаю {name}");
        fetch(client, &url, &dest, spec, |n, t| {
            progress(&label, t.filter(|&v| v > 0).map(|v| 100 * n / v))
        })
        .await?;
    }
    Ok((exe, models.join(whisper), models.join(SILERO)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread};
    const TEST: FileSpec = FileSpec {
        name: "abc",
        bytes: 3,
        hash: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    };
    async fn serve(body: &'static [u8], hang: bool, spec: FileSpec) -> (Result<()>, PathBuf) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = std::io::BufReader::new(&s);
            let mut line = String::new();
            while std::io::BufRead::read_line(&mut request, &mut line).unwrap() > 0 {
                if line == "\r\n" {
                    break;
                }
                line.clear();
            }
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\n")
                .unwrap();
            s.write_all(body).unwrap();
            if hang {
                thread::sleep(Duration::from_millis(1400));
            }
        });
        let dest = crate::paths::test_dir().join("abc");
        let result = fetch(
            &client(Duration::from_secs(1)).unwrap(),
            &format!("http://{addr}"),
            &dest,
            Some(spec),
            |_, _| {},
        )
        .await;
        server.join().unwrap();
        assert!(!dest.with_extension("part").exists());
        (result, dest)
    }
    #[tokio::test]
    async fn success_and_corruption() {
        let (r, p) = serve(b"abc", false, TEST).await;
        r.unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"abc");
        fs::remove_dir_all(p.parent().unwrap()).unwrap();
        for body in [&b"xyz"[..], b"ab"] {
            let (r, p) = serve(body, false, TEST).await;
            assert!(r.is_err());
            assert!(!p.exists());
            fs::remove_dir_all(p.parent().unwrap()).unwrap();
        }
    }
    #[tokio::test]
    async fn stalls_time_out() {
        let (r, p) = serve(b"a", true, TEST).await;
        assert!(format!("{:#}", r.unwrap_err()).contains("таймаут"));
        assert!(!p.exists());
        fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
}
