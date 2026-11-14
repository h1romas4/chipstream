local exports = { name = 'nanodrive6', version = '0.3.0', description = 'Sega System C2 and Mega Drive ND6 Bridge', license = 'BSD-3-Clause', author = { name = 'ND6' } }
local subscriptions = {}
local taps, socket, address, address_page, active, paused, origin = {}
local fast_mode, skipped_dac = false, 0

function exports.startplugin()
    local function tick()
        return math.max(0, (manager.machine.time - origin):as_ticks(44100))
    end
    local function send(data)
        if not active then return end
        if socket:write(data) ~= #data then
            active = false
            emu.print_error('ND6: connection lost or incomplete write; restart bridge and MAME\n')
        end
    end
    local function remove_taps()
        for _, tap in ipairs(taps) do tap:remove() end
        taps = {}
    end
    local function psg_write(value)
        send('G' .. string.pack('<I8B', tick(), value))
    end
    local function ym_write(port, value)
        local page = port >> 1
        if (port & 1) == 0 then
            address, address_page = value, page
        elseif page == address_page then
            if fast_mode and page == 0 and address == 0x2a then
                skipped_dac = math.min(skipped_dac + 1, 0xffffffff)
                return
            end
            -- Every YM write shares the same source timeline, including FM,
            -- DAC enable, and DAC samples. Legacy bridges ignore FM ticks.
            send('Y' .. string.pack('<I8BBB', tick(), page, address, value))
        end
    end

    local function update_fast_mode()
        if not active then return end
        local video = manager.machine.video
        -- Do not call ioport:type_pressed here: input-type entry bindings in
        -- MAME 0.289 can cause a native access violation.
        local fast = not video.throttled or video.speed_percent > 1.2
        if fast ~= fast_mode then
            local count = fast_mode and skipped_dac or 0
            fast_mode = fast
            send('F' .. string.pack('<BI4', fast and 1 or 0, count))
            if not fast then skipped_dac = 0 end
        end
    end

    subscriptions.reset = emu.add_machine_reset_notifier(function()
        local source = (manager.machine.system.source_file or ''):gsub('\\', '/')
        local c2 = source == 'sega/segac2.cpp' or source:match('/sega/segac2%.cpp$')
        local md = source == 'sega/mdconsole.cpp' or source:match('/sega/mdconsole%.cpp$')
        if not c2 and not md then return end
        local cpu = manager.machine.devices[':maincpu']
        local ym = manager.machine.devices[':ymsnd']
        local space = cpu and cpu.spaces['program']
        local z80 = md and manager.machine.devices[':genesis_snd_z80']
        local zspace = z80 and z80.spaces['program']
        if not ym or (ym.shortname ~= 'ym3438' and ym.shortname ~= 'ym2612') or not space or (md and not zspace) then
            emu.print_warning('ND6: compatible YM2612/YM3438 and CPU program spaces not found\n')
            return
        end
        if not socket then
            socket = emu.file('', 3)
            local err = socket:open('socket.127.0.0.1:32151')
            if err then
                emu.print_error('ND6: cannot connect; start bridge with --device nd6: ' .. tostring(err) .. '\n')
                socket = nil
                return
            end
            active = true
        end
        origin = manager.machine.time
        send('R')
        if md then
            -- The version register exposes the emulated console's video standard.
            local pal = (space:read_u8(0xa10000) & 0x40) ~= 0
            send('C' .. string.pack('<I4', pal and 7600489 or 7670454))
        end
        if paused then send('P' .. string.pack('<I8', 0)) end
        fast_mode, skipped_dac = false, 0
        update_fast_mode()
        address, address_page = 0, 0
        remove_taps()
        -- VDP PSG is reached through the low byte of the 68k VDP ports.
        local function vdp_tap(offset, data, mask)
            local canonical = c2 and (offset & (~0x18ff00)) or offset
            if c2 and (canonical < 0xc00000 or canonical > 0xc0001f) then return end
            local port = canonical & 0x1f
            if port >= 0x10 and port <= 0x16 and (port & 1) == 0 and (mask & 0xff) ~= 0 then
                psg_write(data & 0xff)
            end
        end
        taps[#taps + 1] = space:install_write_tap(0xc00000, c2 and 0xd8ff1f or 0xc0001f, 'nd6_psg', vdp_tap)
        if md then
            taps[#taps + 1] = space:install_write_tap(0xd00000, 0xd0001f, 'nd6_psg_mirror', vdp_tap)
            taps[#taps + 1] = zspace:install_write_tap(0x7f11, 0x7f17, 'nd6_z80_psg', function(offset, data, mask)
                local port = offset & 0xff
                if (port == 0x11 or port == 0x13 or port == 0x15 or port == 0x17) and (mask & 0xff) ~= 0 then
                    psg_write(data & 0xff)
                end
            end)
            -- MAME's 68k handler accepts YM writes only while it owns the
            -- Z80 bus and the Z80 reset line is clear. Track both controls.
            local bus_requested, z80_running = false, false
            taps[#taps + 1] = space:install_write_tap(0xa11100, 0xa11101, 'nd6_bus', function(_, data, mask)
                local bit = (mask & 0xff00) ~= 0 and (data & 0x0100) or (data & 1)
                bus_requested = bit ~= 0
            end)
            taps[#taps + 1] = space:install_write_tap(0xa11200, 0xa11201, 'nd6_reset', function(_, data, mask)
                local bit = (mask & 0xff00) ~= 0 and (data & 0x0100) or (data & 1)
                z80_running = bit ~= 0
            end)
            taps[#taps + 1] = space:install_write_tap(0xa04000, 0xa04003, 'nd6_md_68k', function(offset, data, mask)
                if not bus_requested or not z80_running then return end
                if (mask & 0xff00) ~= 0 then ym_write(offset & 3, (data >> 8) & 0xff) end
                if (mask & 0x00ff) ~= 0 then ym_write((offset & 3) + 1, data & 0xff) end
            end)
            taps[#taps + 1] = zspace:install_write_tap(0x4000, 0x5fff, 'nd6_md_z80', function(offset, data, mask)
                if (mask & 0xff) ~= 0 then ym_write(offset & 3, data & 0xff) end
            end)
        else
        -- 68k low byte, eight bytes mapped to four YM ports.  Decode all
        -- mirrors, but ignore unrelated devices within the tapped range.
        taps[#taps + 1] = space:install_write_tap(0x840100, 0x97ffff, 'nd6_ym3438', function(offset, data, mask)
            local canonical = offset & (~0x13fef8)
            if canonical < 0x840100 or canonical > 0x840107 or (mask & 0xff) == 0 then return end
            local port = (canonical & 7) >> 1
            ym_write(port, data & 0xff)
        end)
        end
        print('ND6: YM2612/YM3438 taps installed for ' .. emu.romname() .. ' (' .. source .. ')')
    end)
    emu.register_frame_done(update_fast_mode)
    subscriptions.pause = emu.add_machine_pause_notifier(function()
        paused = true
        if active then send('P' .. string.pack('<I8', 0)) end
    end)
    subscriptions.resume = emu.add_machine_resume_notifier(function()
        paused = false
        if active then send('U' .. string.pack('<I8', 0)) end
    end)
    subscriptions.stop = emu.add_machine_stop_notifier(function()
        send('Q')
        remove_taps()
        if socket then socket:close(); socket = nil end
        active = false
    end)
end
return exports
