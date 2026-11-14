local exports = { name = 'nanodrive8', version = '0.3.0', description = 'MAME YM2151 to ND8 bridge', license = 'BSD-3-Clause', author = { name = 'ND8' } }
local subscriptions = {}
local taps, socket, address, active, paused = {}, nil, 0, false, false

function exports.startplugin()
    local function send(data)
        if not active then return end
        if socket:write(data) ~= #data then
            active = false
            emu.print_error('ND8: connection lost or incomplete write; restart bridge and MAME\n')
        end
    end

    local function remove_taps()
        for _, tap in ipairs(taps) do tap:remove() end
        taps = {}
    end

    subscriptions.reset = emu.add_machine_reset_notifier(function()
        remove_taps()
        local ym, count = nil, 0
        for _, dev in pairs(manager.machine.devices) do
            if dev.shortname == 'ym2151' then ym, count = dev, count + 1 end
        end
        if count == 0 then return end
        -- Address-map handler metadata does not identify the destination
        -- device reliably; do not combine two chips into one ND8.
        if count ~= 1 then
            emu.print_warning('ND8: multiple YM2151 devices are not supported\n')
            return
        end

        local mappings = {}
        local has_pair, has_address, has_data = false, false, false
        for _, dev in pairs(manager.machine.devices) do
            for space_name, space in pairs(dev.spaces) do
                if space.map and (space.data_width == 8 or space.data_width == 16) and space.shift == 0 then
                    for _, entry in ipairs(space.map.entries) do
                        local name = entry.write.name
                        local kind = name == 'ym2151_device::write' and 'pair'
                            or name == 'ym2151_device::address_w' and 'address'
                            or name == 'ym2151_device::data_w' and 'data'
                        local stride = space.data_width // 8
                        local lane = space.data_width == 8 and 0xff or (entry.mask & 0xffff)
                        local shift = lane == 0xff and 0 or lane == 0xff00 and 8 or nil
                        if kind and shift and entry.write.bits == 8 and entry.address_mask == 0
                            and ((kind == 'pair' and entry.address_end == entry.address_start + 2 * stride - 1)
                                or (kind ~= 'pair' and entry.address_end - entry.address_start < stride)) then
                            mappings[#mappings + 1] = { space = space, name = space_name,
                                start = entry.address_start, finish = entry.address_end,
                                mirror = entry.address_mirror, kind = kind, cpu = dev.tag,
                                lane = lane, shift = shift, stride = stride }
                            has_pair = has_pair or kind == 'pair'
                            has_address = has_address or kind == 'address'
                            has_data = has_data or kind == 'data'
                        end
                    end
                end
            end
        end
        if not has_pair and not (has_address and has_data) then
            emu.print_warning('ND8: YM2151 found but no supported direct write map for ' .. emu.romname() .. '\n')
            return
        end
        local game, tag = emu.romname(), ym.tag
        if #game > 255 or #tag > 255 then
            emu.print_error('ND8: machine name or YM2151 tag too long\n')
            return
        end
        if not socket then
            socket = emu.file('', 3)
            local err = socket:open('socket.127.0.0.1:32151')
            if err then
                emu.print_error('ND8: cannot connect to bridge; start bridge before MAME: ' .. tostring(err) .. '\n')
                socket = nil
                return
            end
            active = true
        end
        -- MAME 0.289 does not expose device.clock to Lua. The bridge looks up
        -- this exact machine and device tag with the running MAME's -listxml.
        send('I' .. string.pack('<I4BB', emu.pid(), #game, #tag) .. game .. tag)
        send('R')
        if paused then send('P') end
        address = 0
        for _, map in ipairs(mappings) do
            local limit = map.finish | map.mirror
            taps[#taps + 1] = map.space:install_write_tap(map.start, limit, 'nd8_ym2151', function(offset, data, mask)
                local canonical = offset & (~map.mirror)
                if canonical < map.start or canonical > map.finish or (mask & map.lane) == 0 then return end
                if map.kind == 'address' or (map.kind == 'pair' and canonical == map.start) then
                    address = (data >> map.shift) & 0xff
                else
                    send(string.char(0x57, address, (data >> map.shift) & 0xff))
                end
            end)
            print(string.format('ND8: YM2151 tap %s:%s %x-%x mirror %x (%s)', map.cpu, map.name, map.start, map.finish, map.mirror, map.kind))
        end
        print('ND8: YM2151 ' .. tag .. ' detected for ' .. game)
    end)
    subscriptions.pause = emu.add_machine_pause_notifier(function() paused = true; send('P') end)
    subscriptions.resume = emu.add_machine_resume_notifier(function() paused = false; send('U') end)
    subscriptions.stop = emu.add_machine_stop_notifier(function()
        send('Q')
        remove_taps()
        if socket then socket:close(); socket = nil end
        active = false
    end)
end
return exports
