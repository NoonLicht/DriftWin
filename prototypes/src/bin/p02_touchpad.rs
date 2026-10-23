//! Гипотезы: Precision Touchpad виден как HID 0x0D/0x05, приходит через WM_INPUT с RIDEV_INPUTSINK,
//! отчёты разбираются HidP_*; системные жесты читаются из реестра.
//! Запуск: p02_touchpad [секунды=5]. Для проверки потока контактов нужно трогать тачпад во время запуска.

use std::time::{Duration, Instant};

use dw_prototypes::*;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Input::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

fn dump_registry() {
    println!(
        "-- HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\PrecisionTouchPad (только чтение):"
    );
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\PrecisionTouchPad"),
            None,
            KEY_READ,
            &mut key,
        )
        .is_err()
        {
            println!("  ключ не открылся");
            return;
        }
        for name in [
            w!("ThreeFingerSlideEnabled"),
            w!("ThreeFingerTapEnabled"),
            w!("FourFingerSlideEnabled"),
            w!("FourFingerTapEnabled"),
            w!("ThreeFingerSlideEnabled"),
            w!("AAPThreshold"),
            w!("ScrollDirection"),
        ] {
            let mut v = 0u32;
            let mut sz = 4u32;
            let r = RegQueryValueExW(
                key,
                name,
                None,
                None,
                Some(&mut v as *mut _ as *mut u8),
                Some(&mut sz),
            );
            println!(
                "  {:<26} = {}",
                name.to_string().unwrap_or_default(),
                if r.is_ok() {
                    v.to_string()
                } else {
                    "—".into()
                }
            );
        }
        let _ = RegCloseKey(key);
    }
}

fn find_touchpads() -> Vec<RAWINPUTDEVICELIST> {
    unsafe {
        let mut n = 0u32;
        GetRawInputDeviceList(None, &mut n, size_of::<RAWINPUTDEVICELIST>() as u32);
        let mut v = vec![RAWINPUTDEVICELIST::default(); n as usize];
        GetRawInputDeviceList(
            Some(v.as_mut_ptr()),
            &mut n,
            size_of::<RAWINPUTDEVICELIST>() as u32,
        );
        println!("-- Raw Input устройств: {n}");
        let mut pads = Vec::new();
        for d in v.iter().take(n as usize) {
            if d.dwType != RIM_TYPEHID {
                continue;
            }
            let mut info = RID_DEVICE_INFO {
                cbSize: size_of::<RID_DEVICE_INFO>() as u32,
                ..Default::default()
            };
            let mut sz = info.cbSize;
            if GetRawInputDeviceInfoW(
                Some(d.hDevice),
                RIDI_DEVICEINFO,
                Some(&mut info as *mut _ as *mut _),
                &mut sz,
            ) == u32::MAX
            {
                continue;
            }
            let h = info.Anonymous.hid;
            let is_ptp = h.usUsagePage == 0x0D && h.usUsage == 0x05;
            if is_ptp {
                println!(
                    "  PTP: VID {:04X} PID {:04X} usage {:#x}/{:#x}",
                    h.dwVendorId, h.dwProductId, h.usUsagePage, h.usUsage
                );
                pads.push(*d);
            }
        }
        if pads.is_empty() {
            println!("  Precision Touchpad (0x0D/0x05) НЕ найден среди HID-устройств");
        }
        pads
    }
}

fn describe(dev: &RAWINPUTDEVICELIST) {
    unsafe {
        let mut sz = 0u32;
        GetRawInputDeviceInfoW(Some(dev.hDevice), RIDI_PREPARSEDDATA, None, &mut sz);
        let mut buf = vec![0u8; sz as usize];
        GetRawInputDeviceInfoW(
            Some(dev.hDevice),
            RIDI_PREPARSEDDATA,
            Some(buf.as_mut_ptr() as *mut _),
            &mut sz,
        );
        let pd = PHIDP_PREPARSED_DATA(buf.as_mut_ptr() as isize);
        let mut caps = HIDP_CAPS::default();
        let st = HidP_GetCaps(pd, &mut caps);
        println!(
            "  HidP_GetCaps status={:#x} InputReportByteLength={} NumberInputValueCaps={} NumberInputButtonCaps={}",
            st.0, caps.InputReportByteLength, caps.NumberInputValueCaps, caps.NumberInputButtonCaps
        );
        let mut n = caps.NumberInputValueCaps;
        let mut vc = vec![HIDP_VALUE_CAPS::default(); n as usize];
        if HidP_GetValueCaps(HidP_Input, vc.as_mut_ptr(), &mut n, pd).0 >= 0 {
            for c in vc.iter().take(n as usize) {
                let usage = if c.IsRange {
                    c.Anonymous.Range.UsageMin
                } else {
                    c.Anonymous.NotRange.Usage
                };
                println!(
                    "    value: page {:#x} usage {:#x} link {} logical [{}..{}] physical [{}..{}] unit {:#x} exp {}",
                    c.UsagePage,
                    usage,
                    c.LinkCollection,
                    c.LogicalMin,
                    c.LogicalMax,
                    c.PhysicalMin,
                    c.PhysicalMax,
                    c.Units,
                    c.UnitsExp
                );
            }
        }
    }
}

fn main() {
    init_dpi();
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    dump_registry();
    let pads = find_touchpads();
    for p in &pads {
        describe(p);
    }
    let hwnd = create_test_window_ex(
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        "dw-touchpad sink",
        -3000,
        -3000,
        100,
        100,
        0,
    );
    let rid = RAWINPUTDEVICE {
        usUsagePage: 0x0D,
        usUsage: 0x05,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };
    let reg = unsafe { RegisterRawInputDevices(&[rid], size_of::<RAWINPUTDEVICE>() as u32) };
    println!(
        "-- RegisterRawInputDevices(0x0D/0x05, INPUTSINK) -> {:?}; слушаю {secs} с (трогайте тачпад)",
        reg.is_ok()
    );

    let end = Instant::now() + Duration::from_secs(secs);
    let (mut reports, mut max_contacts, mut first) = (0u32, 0u32, true);
    while Instant::now() < end {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_INPUT {
                    let mut sz = 0u32;
                    let hr = HRAWINPUT(msg.lParam.0 as _);
                    GetRawInputData(
                        hr,
                        RID_INPUT,
                        None,
                        &mut sz,
                        size_of::<RAWINPUTHEADER>() as u32,
                    );
                    let mut buf = vec![0u8; sz as usize];
                    GetRawInputData(
                        hr,
                        RID_INPUT,
                        Some(buf.as_mut_ptr() as *mut _),
                        &mut sz,
                        size_of::<RAWINPUTHEADER>() as u32,
                    );
                    let ri = &*(buf.as_ptr() as *const RAWINPUT);
                    if ri.header.dwType == RIM_TYPEHID.0 {
                        reports += 1;
                        let hid = &ri.data.hid;
                        let report = std::slice::from_raw_parts(
                            hid.bRawData.as_ptr(),
                            hid.dwSizeHid as usize,
                        );
                        // Contact Count: page 0x0D usage 0x54
                        let mut psz = 0u32;
                        GetRawInputDeviceInfoW(
                            Some(ri.header.hDevice),
                            RIDI_PREPARSEDDATA,
                            None,
                            &mut psz,
                        );
                        let mut pb = vec![0u8; psz as usize];
                        GetRawInputDeviceInfoW(
                            Some(ri.header.hDevice),
                            RIDI_PREPARSEDDATA,
                            Some(pb.as_mut_ptr() as *mut _),
                            &mut psz,
                        );
                        let pd = PHIDP_PREPARSED_DATA(pb.as_mut_ptr() as isize);
                        let mut cc = 0u32;
                        let st = HidP_GetUsageValue(
                            HidP_Input,
                            0x0D,
                            Some(0),
                            0x54,
                            &mut cc,
                            pd,
                            report,
                        );
                        if st.0 >= 0 {
                            max_contacts = max_contacts.max(cc);
                        }
                        if first {
                            first = false;
                            println!(
                                "  первый отчёт: {} байт, hex {:02x?}, ContactCount status={:#x} значение={cc}",
                                report.len(),
                                &report[..report.len().min(24)],
                                st.0
                            );
                        }
                    }
                } else {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    println!(
        "-- Принято WM_INPUT HID-отчётов: {reports}; максимум одновременных контактов (Contact Count): {max_contacts}"
    );
    let _ = (HWND::default(), WPARAM(0), LPARAM(0));
}
