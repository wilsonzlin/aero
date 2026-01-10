use crate::wim::Arch;
use anyhow::{anyhow, Result};
use clap::ValueEnum;

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnattendMode {
    None,
    DriversOnly,
    Full,
}

pub fn render_autounattend(
    arch: Arch,
    driver_dir_rel: &str,
    mode: UnattendMode,
) -> Result<String> {
    match mode {
        UnattendMode::None => Err(anyhow!("UnattendMode::None does not render a file")),
        UnattendMode::DriversOnly => Ok(render_drivers_only(arch, driver_dir_rel)),
        UnattendMode::Full => Ok(render_full(arch, driver_dir_rel)),
    }
}

fn render_drivers_only(arch: Arch, driver_dir_rel: &str) -> String {
    let arch_name = arch.unattend_processor_arch();
    let driver_path = format!("%configsetroot%\\\\{}", driver_dir_rel.replace('/', "\\\\"));

    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend">
  <settings pass="windowsPE">
    <component name="Microsoft-Windows-PnpCustomizationsWinPE" processorArchitecture="{arch_name}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <DriverPaths>
        <PathAndCredentials wcm:action="add" wcm:keyValue="1">
          <Path>{driver_path}</Path>
        </PathAndCredentials>
      </DriverPaths>
    </component>
  </settings>
  <settings pass="offlineServicing">
    <component name="Microsoft-Windows-PnpCustomizationsNonWinPE" processorArchitecture="{arch_name}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <DriverPaths>
        <PathAndCredentials wcm:action="add" wcm:keyValue="1">
          <Path>{driver_path}</Path>
        </PathAndCredentials>
      </DriverPaths>
    </component>
  </settings>
</unattend>
"#,
        arch_name = arch_name,
        driver_path = driver_path
    )
}

fn render_full(arch: Arch, driver_dir_rel: &str) -> String {
    // Full unattended installs are extremely environment-specific (disk layout, image selection,
    // locale, product key, etc). We keep this mode intentionally conservative and focus on
    // "Aero-necessary" settings only (driver staging), plus a minimal EULA acceptance.
    let arch_name = arch.unattend_processor_arch();
    let driver_path = format!("%configsetroot%\\\\{}", driver_dir_rel.replace('/', "\\\\"));

    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend">
  <settings pass="windowsPE">
    <component name="Microsoft-Windows-Setup" processorArchitecture="{arch_name}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <UserData>
        <AcceptEula>true</AcceptEula>
      </UserData>
    </component>
    <component name="Microsoft-Windows-PnpCustomizationsWinPE" processorArchitecture="{arch_name}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <DriverPaths>
        <PathAndCredentials wcm:action="add" wcm:keyValue="1">
          <Path>{driver_path}</Path>
        </PathAndCredentials>
      </DriverPaths>
    </component>
  </settings>
  <settings pass="offlineServicing">
    <component name="Microsoft-Windows-PnpCustomizationsNonWinPE" processorArchitecture="{arch_name}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <DriverPaths>
        <PathAndCredentials wcm:action="add" wcm:keyValue="1">
          <Path>{driver_path}</Path>
        </PathAndCredentials>
      </DriverPaths>
    </component>
  </settings>
</unattend>
"#,
        arch_name = arch_name,
        driver_path = driver_path
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_drivers_only_template() {
        let xml = render_autounattend(Arch::X64, "AERO/DRIVERS/amd64", UnattendMode::DriversOnly)
            .unwrap();
        assert!(xml.contains("Microsoft-Windows-PnpCustomizationsWinPE"));
        assert!(xml.contains("%configsetroot%\\\\AERO\\\\DRIVERS\\\\amd64"));
        assert!(xml.contains("processorArchitecture=\"amd64\""));
    }
}

