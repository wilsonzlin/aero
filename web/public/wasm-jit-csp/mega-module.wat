(module
  (memory (export "memory") 1)

  ;; Very small “mega-module” / bytecode engine, used as a fallback when
  ;; dynamic WebAssembly module compilation is blocked by CSP.
  ;;
  ;; Bytecode format (little-endian immediates):
  ;; - 0x01: PUSH_I32 <i32>
  ;; - 0x02: ADD
  ;; - 0x03: MUL
  ;; - 0x04: RETURN
  ;;
  ;; Stack is stored in linear memory starting at STACK_BASE.
  (func (export "run") (param $ptr i32) (param $len i32) (result i32)
    (local $ip i32)
    (local $end i32)
    (local $sp i32)
    (local $op i32)
    (local $a i32)
    (local $b i32)
    (local $val i32)

    local.get $ptr
    local.set $ip

    local.get $ptr
    local.get $len
    i32.add
    local.set $end

    i32.const 4096
    local.set $sp

    (block $exit
      (loop $loop
        local.get $ip
        local.get $end
        i32.ge_u
        br_if $exit

        local.get $ip
        i32.load8_u
        local.set $op

        local.get $ip
        i32.const 1
        i32.add
        local.set $ip

        local.get $op
        i32.const 1
        i32.eq
        if
          ;; PUSH_I32
          local.get $ip
          i32.load
          local.set $val

          local.get $ip
          i32.const 4
          i32.add
          local.set $ip

          local.get $sp
          local.get $val
          i32.store

          local.get $sp
          i32.const 4
          i32.add
          local.set $sp
        else
          local.get $op
          i32.const 2
          i32.eq
          if
            ;; ADD
            local.get $sp
            i32.const 4
            i32.sub
            local.set $sp
            local.get $sp
            i32.load
            local.set $a

            local.get $sp
            i32.const 4
            i32.sub
            local.set $sp
            local.get $sp
            i32.load
            local.set $b

            local.get $sp
            local.get $b
            local.get $a
            i32.add
            i32.store

            local.get $sp
            i32.const 4
            i32.add
            local.set $sp
          else
            local.get $op
            i32.const 3
            i32.eq
            if
              ;; MUL
              local.get $sp
              i32.const 4
              i32.sub
              local.set $sp
              local.get $sp
              i32.load
              local.set $a

              local.get $sp
              i32.const 4
              i32.sub
              local.set $sp
              local.get $sp
              i32.load
              local.set $b

              local.get $sp
              local.get $b
              local.get $a
              i32.mul
              i32.store

              local.get $sp
              i32.const 4
              i32.add
              local.set $sp
            else
              local.get $op
              i32.const 4
              i32.eq
              if
                ;; RETURN
                local.get $sp
                i32.const 4
                i32.sub
                local.set $sp
                local.get $sp
                i32.load
                return
              else
                unreachable
              end
            end
          end
        end

        br $loop
      )
    )

    ;; If we fall off the end, the program is invalid.
    unreachable
  )
)

